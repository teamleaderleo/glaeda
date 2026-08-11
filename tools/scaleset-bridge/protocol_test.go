package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"reflect"
	"strings"
	"testing"

	"github.com/actions/scaleset"
)

type fakeBackend struct {
	initial       *scaleset.RunnerScaleSetStatistic
	message       *scaleset.RunnerScaleSetMessage
	jit           *scaleset.RunnerScaleSetJitRunnerConfig
	runner        *scaleset.RunnerReference
	acquired      []int64
	calls         []string
	lastMessageID int
	maxCapacity   int
	validation    []error
}

func (backend *fakeBackend) Poll(_ context.Context, lastMessageID, maxCapacity int) (*scaleset.RunnerScaleSetMessage, error) {
	backend.calls = append(backend.calls, "poll")
	backend.lastMessageID = lastMessageID
	backend.maxCapacity = maxCapacity
	message := backend.message
	backend.message = nil
	return message, nil
}
func (backend *fakeBackend) DeleteMessage(_ context.Context, messageID int) error {
	backend.calls = append(backend.calls, fmt.Sprintf("delete:%d", messageID))
	return nil
}
func (backend *fakeBackend) AcquireJobs(_ context.Context, requestIDs []int64) ([]int64, error) {
	backend.calls = append(backend.calls, fmt.Sprintf("acquire:%v", requestIDs))
	return append([]int64(nil), backend.acquired...), nil
}
func (backend *fakeBackend) GenerateJIT(_ context.Context, name, workFolder string) (*scaleset.RunnerScaleSetJitRunnerConfig, error) {
	backend.calls = append(backend.calls, fmt.Sprintf("jit:%s:%s", name, workFolder))
	return backend.jit, nil
}
func (backend *fakeBackend) GetRunner(_ context.Context, id int) (*scaleset.RunnerReference, error) {
	backend.calls = append(backend.calls, fmt.Sprintf("runner-id:%d", id))
	return backend.runner, nil
}
func (backend *fakeBackend) GetRunnerByName(_ context.Context, name string) (*scaleset.RunnerReference, error) {
	backend.calls = append(backend.calls, "runner:"+name)
	return backend.runner, nil
}
func (backend *fakeBackend) RemoveRunner(_ context.Context, id int64) error {
	backend.calls = append(backend.calls, fmt.Sprintf("remove:%d", id))
	return nil
}
func (backend *fakeBackend) ValidateScaleSet(context.Context) error {
	backend.calls = append(backend.calls, "validate-set")
	if len(backend.validation) == 0 {
		return nil
	}
	err := backend.validation[0]
	backend.validation = backend.validation[1:]
	return err
}
func (backend *fakeBackend) InitialStatistics() *scaleset.RunnerScaleSetStatistic {
	return backend.initial
}
func (backend *fakeBackend) Close(context.Context) error {
	backend.calls = append(backend.calls, "close")
	return nil
}

func validStart() startConfig {
	return startConfig{
		GitHubConfigURL: "https://github.com/example/project",
		ClientID:        "Iv1.example",
		InstallationID:  17,
		PrivateKey:      "-----BEGIN PRIVATE KEY-----\nprivate\n-----END PRIVATE KEY-----",
		ScaleSetID:      23,
		ScaleSetName:    "smolrunner",
		RunnerGroupID:   1,
		Labels:          []string{"smolrunner"},
		Owner:           "smolrunner-host",
		MaxCapacity:     1,
	}
}

func startedServer(t *testing.T, fake *fakeBackend) *server {
	t.Helper()
	if fake.initial == nil {
		fake.initial = &scaleset.RunnerScaleSetStatistic{}
	}
	server := newServer(func(context.Context, startConfig) (backend, error) {
		return fake, nil
	})
	response := server.handle(context.Background(), protocolRequest{
		Version:   protocolVersion,
		Operation: "start",
		Start:     validStart(),
	})
	if response.Type != "ready" {
		t.Fatalf("start response = %#v", response)
	}
	return server
}

func TestDecodeRequestIsStrictAndVersioned(t *testing.T) {
	valid := []byte(`{"version":1,"operation":"poll"}`)
	request, err := decodeRequest(valid)
	if err != nil || request.Operation != "poll" {
		t.Fatalf("valid request: request=%#v err=%v", request, err)
	}
	for _, input := range [][]byte{
		[]byte(`{"version":2,"operation":"poll"}`),
		[]byte(`{"version":1,"operation":"poll","unknown":true}`),
		[]byte(`{"version":1,"operation":"poll"} {}`),
	} {
		if _, err := decodeRequest(input); err == nil {
			t.Fatalf("accepted invalid request %s", input)
		}
	}
}

func TestPollRequiresDurableAckBeforeAdvancing(t *testing.T) {
	backend := &fakeBackend{
		initial:  &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 1},
		acquired: []int64{41},
		message: &scaleset.RunnerScaleSetMessage{
			MessageID: 7,
			Statistics: &scaleset.RunnerScaleSetStatistic{
				TotalAvailableJobs: 1,
				TotalAssignedJobs:  1,
			},
			JobAvailableMessages: []*scaleset.JobAvailable{{
				JobMessageBase: scaleset.JobMessageBase{
					RunnerRequestID: 41,
					RepositoryName:  "project",
					OwnerName:       "example",
					JobID:           "job-1",
					WorkflowRunID:   99,
					RequestLabels:   []string{"smolrunner"},
				},
			}},
		},
	}
	server := startedServer(t, backend)

	message := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll", MaxCapacity: 1})
	if message.Type != "message" || message.MessageID != 7 || len(message.Events) != 1 {
		t.Fatalf("message response = %#v", message)
	}
	if response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll", MaxCapacity: 1}); response.Code != "ack_required" {
		t.Fatalf("second poll response = %#v", response)
	}
	if !reflect.DeepEqual(backend.calls, []string{"poll"}) {
		t.Fatalf("calls before ack = %v", backend.calls)
	}

	acked := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "ack", MessageID: 7})
	if acked.Type != "acked" || !reflect.DeepEqual(acked.AcquiredRequests, []int64{41}) {
		t.Fatalf("ack response = %#v", acked)
	}
	if !reflect.DeepEqual(backend.calls, []string{"poll", "delete:7", "acquire:[41]"}) {
		t.Fatalf("ack ordering = %v", backend.calls)
	}
	if server.lastAckedID != 7 || server.pending != nil || backend.maxCapacity != 1 {
		t.Fatalf("server did not advance exact ack state")
	}
}

func TestResumeRefetchesExactDurablePendingBeforeAck(t *testing.T) {
	backend := &fakeBackend{
		acquired: []int64{41},
		message: &scaleset.RunnerScaleSetMessage{
			MessageID:  8,
			Statistics: &scaleset.RunnerScaleSetStatistic{TotalAvailableJobs: 1},
			JobAvailableMessages: []*scaleset.JobAvailable{{JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: 41,
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           "job-1",
				WorkflowRunID:   99,
				RequestLabels:   []string{"smolrunner"},
			}}},
		},
	}
	server := startedServer(t, backend)
	resumed := server.handle(context.Background(), protocolRequest{
		Version:     1,
		Operation:   "resume",
		LastAckedID: 7,
		MessageID:   8,
		MaxCapacity: 1,
	})
	if resumed.Type != "message" || resumed.MessageID != 8 || server.pending == nil || server.lastAckedID != 7 {
		t.Fatalf("resume response=%#v pending=%#v last=%d", resumed, server.pending, server.lastAckedID)
	}
	acked := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "ack", MessageID: 8})
	if acked.Type != "acked" || !reflect.DeepEqual(acked.AcquiredRequests, []int64{41}) {
		t.Fatalf("ack response=%#v", acked)
	}
	if !reflect.DeepEqual(backend.calls, []string{"poll", "delete:8", "acquire:[41]"}) || backend.lastMessageID != 7 {
		t.Fatalf("resume/ack calls=%v last=%d", backend.calls, backend.lastMessageID)
	}
}

func TestResumeRefusesMismatchedRedeliveryWithoutBinding(t *testing.T) {
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{
		MessageID:  9,
		Statistics: &scaleset.RunnerScaleSetStatistic{},
	}}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{
		Version:     1,
		Operation:   "resume",
		LastAckedID: 7,
		MessageID:   8,
	})
	if response.Code != "recovery_failed" || server.pending != nil || server.lastAckedID != 0 || server.recoveryApplied {
		t.Fatalf("mismatched resume response=%#v pending=%#v last=%d", response, server.pending, server.lastAckedID)
	}
}

func TestResumeRestoresAcknowledgedCursorWithoutPolling(t *testing.T) {
	backend := &fakeBackend{}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{
		Version:     1,
		Operation:   "resume",
		LastAckedID: 7,
	})
	if response.Type != "restored" || response.MessageID != 7 || server.lastAckedID != 7 || !server.recoveryApplied {
		t.Fatalf("cursor restore response=%#v last=%d", response, server.lastAckedID)
	}
	if len(backend.calls) != 0 {
		t.Fatalf("cursor-only restore unexpectedly called backend: %v", backend.calls)
	}
}

func TestIdleRetainsLatestValidatedStatistics(t *testing.T) {
	backend := &fakeBackend{
		initial: &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 8},
		message: &scaleset.RunnerScaleSetMessage{
			MessageID:  7,
			Statistics: &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 3},
		},
	}
	server := startedServer(t, backend)
	message := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if message.Type != "message" || message.Statistics == nil || message.Statistics.AssignedJobs != 3 {
		t.Fatalf("message response=%#v", message)
	}
	if response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "ack", MessageID: 7}); response.Type != "acked" {
		t.Fatalf("ack response=%#v", response)
	}
	idle := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if idle.Type != "idle" || idle.Statistics == nil || idle.Statistics.AssignedJobs != 3 {
		t.Fatalf("idle reverted to startup statistics: %#v", idle)
	}
}

func TestPollUsesCurrentAvailableCapacity(t *testing.T) {
	backend := &fakeBackend{}
	server := startedServer(t, backend)

	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll", MaxCapacity: 0})
	if response.Type != "idle" || backend.maxCapacity != 0 {
		t.Fatalf("zero-capacity poll response=%#v capacity=%d", response, backend.maxCapacity)
	}
	response = server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll", MaxCapacity: 2})
	if response.Code != "invalid_capacity" || !reflect.DeepEqual(backend.calls, []string{"poll"}) {
		t.Fatalf("widened capacity response=%#v calls=%v", response, backend.calls)
	}
}

func TestPollRejectsAvailableJobsBeyondCurrentCapacity(t *testing.T) {
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{
		MessageID:  7,
		Statistics: &scaleset.RunnerScaleSetStatistic{TotalAvailableJobs: 1},
		JobAvailableMessages: []*scaleset.JobAvailable{{JobMessageBase: scaleset.JobMessageBase{
			RunnerRequestID: 41,
			RepositoryName:  "project",
			OwnerName:       "example",
			JobID:           "job-1",
			WorkflowRunID:   99,
			RequestLabels:   []string{"smolrunner"},
		}}},
	}}
	server := startedServer(t, backend)

	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll", MaxCapacity: 0})
	if response.Code != "invalid_message" || server.pending != nil || !reflect.DeepEqual(backend.calls, []string{"poll"}) {
		t.Fatalf("over-capacity message response=%#v pending=%#v calls=%v", response, server.pending, backend.calls)
	}
}

func TestAckRejectsForeignOrDuplicateAcquisitionWithoutRepeatingDelete(t *testing.T) {
	backend := &fakeBackend{
		acquired: []int64{42},
		message: &scaleset.RunnerScaleSetMessage{
			MessageID:  7,
			Statistics: &scaleset.RunnerScaleSetStatistic{TotalAvailableJobs: 1},
			JobAvailableMessages: []*scaleset.JobAvailable{{JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: 41,
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           "job-1",
				WorkflowRunID:   99,
				RequestLabels:   []string{"smolrunner"},
			}}},
		},
	}
	server := startedServer(t, backend)
	if response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll", MaxCapacity: 1}); response.Type != "message" {
		t.Fatalf("poll response=%#v", response)
	}
	invalid := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "ack", MessageID: 7})
	if invalid.Code != "invalid_acquisition" || server.pending == nil || !server.pending.deleted || server.lastAckedID != 0 {
		t.Fatalf("invalid acquisition advanced state: response=%#v pending=%#v", invalid, server.pending)
	}
	backend.acquired = []int64{41}
	valid := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "ack", MessageID: 7})
	if valid.Type != "acked" || !reflect.DeepEqual(valid.AcquiredRequests, []int64{41}) || !reflect.DeepEqual(backend.calls, []string{"poll", "delete:7", "acquire:[41]", "acquire:[41]"}) {
		t.Fatalf("acquisition retry response=%#v calls=%v", valid, backend.calls)
	}
}

func TestAcquiredJobIDsArePositiveUniqueSubset(t *testing.T) {
	if err := validateAcquiredJobs([]int64{41, 42}, []int64{42}); err != nil {
		t.Fatalf("valid subset refused: %v", err)
	}
	for _, acquired := range [][]int64{{0}, {43}, {41, 41}} {
		if err := validateAcquiredJobs([]int64{41, 42}, acquired); err == nil {
			t.Fatalf("invalid acquired IDs accepted: %v", acquired)
		}
	}
}

func TestPollRejectsNonAdvancingMessageID(t *testing.T) {
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{MessageID: 7, Statistics: &scaleset.RunnerScaleSetStatistic{}}}
	server := startedServer(t, backend)
	if response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"}); response.Type != "message" {
		t.Fatalf("first poll response=%#v", response)
	}
	if response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "ack", MessageID: 7}); response.Type != "acked" {
		t.Fatalf("ack response=%#v", response)
	}
	backend.message = &scaleset.RunnerScaleSetMessage{MessageID: 7, Statistics: &scaleset.RunnerScaleSetStatistic{}}
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if response.Code != "invalid_message" || server.pending != nil || server.lastAckedID != 7 {
		t.Fatalf("nonadvancing message response=%#v pending=%#v", response, server.pending)
	}
}

func TestJITSecretIsReturnedOnlyForExactRunner(t *testing.T) {
	backend := &fakeBackend{
		jit: &scaleset.RunnerScaleSetJitRunnerConfig{
			Runner: &scaleset.RunnerReference{
				ID:               81,
				Name:             "smolrunner-job-1",
				RunnerScaleSetID: 23,
			},
			EncodedJITConfig: "one-time-secret",
		},
	}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{
		Version:    1,
		Operation:  "generate_jit",
		RunnerName: "smolrunner-job-1",
		WorkFolder: "_work",
	})
	if response.Type != "jit" || response.EncodedJITConfig != "one-time-secret" || response.Runner.ID != 81 {
		t.Fatalf("jit response = %#v", response)
	}
	encoded, err := json.Marshal(response)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(encoded, []byte(validStart().PrivateKey)) {
		t.Fatal("JIT response contains controller private key")
	}

	backend.jit.Runner.Name = "replacement"
	refused := server.handle(context.Background(), protocolRequest{
		Version:    1,
		Operation:  "generate_jit",
		RunnerName: "smolrunner-job-1",
	})
	if refused.Code != "jit_failed" || refused.EncodedJITConfig != "" {
		t.Fatalf("mismatched JIT response = %#v", refused)
	}

	backend.jit.Runner.Name = "smolrunner-job-1"
	backend.jit.Runner.ID = 0
	refused = server.handle(context.Background(), protocolRequest{
		Version:    1,
		Operation:  "generate_jit",
		RunnerName: "smolrunner-job-1",
	})
	if refused.Code != "jit_failed" || refused.Runner != nil || refused.EncodedJITConfig != "" {
		t.Fatalf("unassigned JIT response = %#v", refused)
	}
}

func TestJITRevalidatesScaleSetPolicyBeforeAndAfterMutation(t *testing.T) {
	backend := &fakeBackend{
		jit: &scaleset.RunnerScaleSetJitRunnerConfig{
			Runner:           &scaleset.RunnerReference{ID: 81, Name: "smolrunner-job-1", RunnerScaleSetID: 23},
			EncodedJITConfig: "one-time-secret",
		},
		validation: []error{nil, errors.New("injected post-JIT drift")},
	}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "generate_jit", RunnerName: "smolrunner-job-1", WorkFolder: "_work"})
	if response.Code != "scale_set_drift" || response.EncodedJITConfig != "" || response.Runner != nil || !reflect.DeepEqual(backend.calls, []string{"validate-set", "jit:smolrunner-job-1:_work", "validate-set"}) {
		t.Fatalf("post-JIT drift response=%#v calls=%v", response, backend.calls)
	}
}

func TestRawMessageGateRejectsUnknownLifecycleBeforeOfficialParser(t *testing.T) {
	statistics := `{"totalAvailableJobs":0,"totalAcquiredJobs":0,"totalAssignedJobs":0,"totalRunningJobs":0,"totalRegisteredRunners":0,"totalBusyRunners":0,"totalIdleRunners":0}`
	knownBody, err := json.Marshal([]map[string]any{{
		"messageType":     "JobCompleted",
		"runnerRequestId": 41,
		"repositoryName":  "project",
		"ownerName":       "example",
		"jobId":           "job-1",
		"workflowRunId":   99,
		"requestLabels":   []string{"smolrunner"},
		"result":          "canceled",
	}})
	if err != nil {
		t.Fatal(err)
	}
	knownEnvelope, err := json.Marshal(map[string]any{"messageId": 7, "messageType": "RunnerScaleSetJobMessages", "body": string(knownBody), "statistics": json.RawMessage(statistics)})
	if err != nil {
		t.Fatal(err)
	}
	if err := validateScaleSetMessageBytes(knownEnvelope); err != nil {
		t.Fatalf("known message refused: %v", err)
	}

	unknownBody, err := json.Marshal([]map[string]any{{"messageType": "JobReassigned"}})
	if err != nil {
		t.Fatal(err)
	}
	unknownEnvelope, err := json.Marshal(map[string]any{"messageId": 8, "messageType": "RunnerScaleSetJobMessages", "body": string(unknownBody), "statistics": json.RawMessage(statistics)})
	if err != nil {
		t.Fatal(err)
	}
	if err := validateScaleSetMessageBytes(unknownEnvelope); err == nil {
		t.Fatal("unknown lifecycle type crossed strict raw-message gate")
	}
}

func TestRetryableClientRunsRawGateAndRestoresMessageBody(t *testing.T) {
	statistics := `{"totalAvailableJobs":0,"totalAcquiredJobs":0,"totalAssignedJobs":0,"totalRunningJobs":0,"totalRegisteredRunners":0,"totalBusyRunners":0,"totalIdleRunners":0}`
	unknownBody, err := json.Marshal([]map[string]any{{"messageType": "JobReassigned"}})
	if err != nil {
		t.Fatal(err)
	}
	envelope, err := json.Marshal(map[string]any{"messageId": 8, "messageType": "RunnerScaleSetJobMessages", "body": string(unknownBody), "statistics": json.RawMessage(statistics)})
	if err != nil {
		t.Fatal(err)
	}
	request, err := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://example.invalid/messages", nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set(scaleset.HeaderScaleSetMaxCapacity, "1")
	response := &http.Response{StatusCode: http.StatusOK, Request: request, Body: io.NopCloser(bytes.NewReader(envelope))}
	retry, gateErr := newStrictHTTPClient().CheckRetry(context.Background(), response, nil)
	if retry || gateErr == nil {
		t.Fatalf("unknown raw lifecycle was not refused: retry=%t err=%v", retry, gateErr)
	}
	restored, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(restored, envelope) {
		t.Fatal("raw gate did not restore exact response body")
	}
}

func TestScaleSetValidationBindsIdentityLabelsAndUpdatePolicy(t *testing.T) {
	config := validStart()
	set := &scaleset.RunnerScaleSet{
		ID:            config.ScaleSetID,
		Name:          config.ScaleSetName,
		RunnerGroupID: config.RunnerGroupID,
		Labels:        []scaleset.Label{{Name: "smolrunner"}},
		RunnerSetting: scaleset.RunnerSetting{DisableUpdate: true},
	}
	if err := validateScaleSet(set, config); err != nil {
		t.Fatal(err)
	}
	set.RunnerSetting.DisableUpdate = false
	if err := validateScaleSet(set, config); err == nil {
		t.Fatal("accepted scale set with runner auto-update enabled")
	}
}

func TestRunnerRemovalReobservesExactScaleSetIdentity(t *testing.T) {
	backend := &fakeBackend{runner: &scaleset.RunnerReference{ID: 81, Name: "smolrunner-job-1", RunnerScaleSetID: 23}}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "remove_runner", RunnerID: 81, RunnerName: "smolrunner-job-1"})
	if response.Type != "removed" || response.Runner == nil || response.Runner.ID != 81 || response.Runner.Name != "smolrunner-job-1" || response.Runner.ScaleSetID != 23 || !reflect.DeepEqual(backend.calls, []string{"runner-id:81", "remove:81"}) {
		t.Fatalf("remove response=%#v calls=%v", response, backend.calls)
	}

	backend.calls = nil
	backend.runner.RunnerScaleSetID = 24
	refused := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "remove_runner", RunnerID: 81, RunnerName: "smolrunner-job-1"})
	if refused.Code != "runner_unavailable" || !reflect.DeepEqual(backend.calls, []string{"runner-id:81"}) {
		t.Fatalf("foreign remove response=%#v calls=%v", refused, backend.calls)
	}

	backend.calls = nil
	backend.runner.RunnerScaleSetID = 23
	refused = server.handle(context.Background(), protocolRequest{Version: 1, Operation: "remove_runner", RunnerID: 81, RunnerName: "replacement"})
	if refused.Code != "runner_unavailable" || !reflect.DeepEqual(backend.calls, []string{"runner-id:81"}) {
		t.Fatalf("name-rebound remove response=%#v calls=%v", refused, backend.calls)
	}
}

func TestRunnerObservationDistinguishesProvenAbsenceFromFailure(t *testing.T) {
	backend := &fakeBackend{}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "observe_runner", RunnerName: "smolrunner-job-1"})
	if response.Type != "runner_absent" || response.Code != "" || response.Runner != nil || !reflect.DeepEqual(backend.calls, []string{"runner:smolrunner-job-1"}) {
		t.Fatalf("absent response=%#v calls=%v", response, backend.calls)
	}

	backend.calls = nil
	backend.runner = &scaleset.RunnerReference{ID: 81, Name: "smolrunner-job-1", RunnerScaleSetID: 23}
	response = server.handle(context.Background(), protocolRequest{Version: 1, Operation: "observe_runner", RunnerName: "smolrunner-job-1"})
	if response.Type != "runner" || response.Runner == nil || response.Runner.ID != 81 || !reflect.DeepEqual(backend.calls, []string{"runner:smolrunner-job-1"}) {
		t.Fatalf("present response=%#v calls=%v", response, backend.calls)
	}
}

func TestMessageBoundsFailBeforeAckAuthority(t *testing.T) {
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{
		MessageID:  7,
		Statistics: &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 1},
		JobStartedMessages: []*scaleset.JobStarted{{
			RunnerID:   81,
			RunnerName: "smolrunner-job-1",
			JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: 41,
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           strings.Repeat("x", 257),
				WorkflowRunID:   99,
				RequestLabels:   []string{"smolrunner"},
			},
		}},
	}}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if response.Code != "invalid_message" || server.pending != nil || !reflect.DeepEqual(backend.calls, []string{"poll"}) {
		t.Fatalf("invalid message response=%#v calls=%v", response, backend.calls)
	}
}

func TestSerializedMessageBoundFailsBeforeAckAuthority(t *testing.T) {
	labels := make([]string, 32)
	for index := range labels {
		labels[index] = strings.Repeat("\\", 100)
	}
	events := make([]*scaleset.JobStarted, 50)
	for index := range events {
		events[index] = &scaleset.JobStarted{
			RunnerID:   index + 1,
			RunnerName: fmt.Sprintf("smolrunner-%d", index+1),
			JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: int64(index + 1),
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           fmt.Sprintf("job-%d", index+1),
				WorkflowRunID:   int64(index + 1),
				RequestLabels:   labels,
			},
		}
	}
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{
		MessageID:          7,
		Statistics:         &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 50, TotalRunningJobs: 50},
		JobStartedMessages: events,
	}}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if response.Code != "invalid_message" || server.pending != nil {
		t.Fatalf("oversized serialized message response=%#v pending=%#v", response, server.pending)
	}
}

func TestMessageRequiresExactRunnerLifecycleEvidenceBeforeAck(t *testing.T) {
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{
		MessageID:  7,
		Statistics: &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 1},
		JobStartedMessages: []*scaleset.JobStarted{{
			JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: 41,
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           "job-1",
				WorkflowRunID:   99,
				RequestLabels:   []string{"smolrunner"},
			},
		}},
	}}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if response.Code != "invalid_message" || server.pending != nil || !reflect.DeepEqual(backend.calls, []string{"poll"}) {
		t.Fatalf("incomplete start response=%#v calls=%v", response, backend.calls)
	}

	backend.message = &scaleset.RunnerScaleSetMessage{
		MessageID:  8,
		Statistics: &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: 1},
		JobCompletedMessages: []*scaleset.JobCompleted{{
			RunnerID:   81,
			RunnerName: "smolrunner-job-1",
			JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: 41,
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           "job-1",
				WorkflowRunID:   99,
				RequestLabels:   []string{"smolrunner"},
			},
		}},
	}
	response = server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if response.Code != "invalid_message" || server.pending != nil {
		t.Fatalf("incomplete completion response=%#v", response)
	}
}

func TestMessageAdmitsExactRunnerlessReassignmentCancellation(t *testing.T) {
	backend := &fakeBackend{message: &scaleset.RunnerScaleSetMessage{
		MessageID:  11,
		Statistics: &scaleset.RunnerScaleSetStatistic{},
		JobCompletedMessages: []*scaleset.JobCompleted{{
			Result: "canceled",
			JobMessageBase: scaleset.JobMessageBase{
				RunnerRequestID: 41,
				RepositoryName:  "project",
				OwnerName:       "example",
				JobID:           "job-1",
				WorkflowRunID:   99,
				RequestLabels:   []string{"smolrunner"},
			},
		}},
	}}
	server := startedServer(t, backend)
	response := server.handle(context.Background(), protocolRequest{Version: 1, Operation: "poll"})
	if response.Type != "message" || response.MessageID != 11 || len(response.Events) != 1 {
		t.Fatalf("runnerless cancellation response=%#v", response)
	}
	event := response.Events[0]
	if event.Kind != "completed" || event.RunnerID != 0 || event.RunnerName != "" || event.Result != "canceled" || server.pending == nil {
		t.Fatalf("runnerless cancellation event=%#v pending=%#v", event, server.pending)
	}
}

func TestServeNeverWritesPrivateKeyOnStartFailure(t *testing.T) {
	server := newServer(func(context.Context, startConfig) (backend, error) {
		return nil, errors.New("injected start failure containing private key")
	})
	request := protocolRequest{Version: 1, Operation: "start", Start: validStart()}
	input, err := json.Marshal(request)
	if err != nil {
		t.Fatal(err)
	}
	input = append(input, '\n')
	var output bytes.Buffer
	if err := serve(context.Background(), bytes.NewReader(input), &output, server); err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(output.Bytes(), []byte(validStart().PrivateKey)) {
		t.Fatalf("error response leaked private key: %s", output.Bytes())
	}
	if !bytes.Contains(output.Bytes(), []byte(`"code":"start_failed"`)) {
		t.Fatalf("unexpected response: %s", output.Bytes())
	}
}
