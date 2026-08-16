package main

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"

	"github.com/hashicorp/go-retryablehttp"
)

type failingRoundTripper struct {
	calls atomic.Int32
}

func (transport *failingRoundTripper) RoundTrip(*http.Request) (*http.Response, error) {
	transport.calls.Add(1)
	return nil, errors.New("synthetic transport failure")
}

func TestStrictHTTPClientDisablesAutomaticRetries(t *testing.T) {
	client := newStrictHTTPClient()
	if client.RetryMax != 0 {
		t.Fatalf("automatic retry count = %d, want 0", client.RetryMax)
	}
}

func TestStrictHTTPClientDoesNotReplayMutationAfterServerFailure(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
		calls.Add(1)
		if request.Method != http.MethodPost {
			t.Errorf("method = %s, want POST", request.Method)
		}
		response.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer server.Close()

	client := newStrictHTTPClient()
	client.Logger = nil
	request, err := retryablehttp.NewRequest(http.MethodPost, server.URL+"/generatejitconfig", []byte(`{"runner":"one"}`))
	if err != nil {
		t.Fatal(err)
	}
	response, err := client.Do(request)
	if response != nil {
		response.Body.Close()
	}
	if err == nil {
		t.Fatal("503 mutation response unexpectedly succeeded")
	}
	if got := calls.Load(); got != 1 {
		t.Fatalf("mutation request count = %d, want exactly 1", got)
	}
}

func TestStrictHTTPClientDoesNotReplayMutationAfterTransportFailure(t *testing.T) {
	transport := &failingRoundTripper{}
	client := newStrictHTTPClient()
	client.Logger = nil
	client.HTTPClient.Transport = transport

	request, err := retryablehttp.NewRequest(http.MethodPost, "https://github.invalid/_apis/runtime/generatejitconfig", []byte(`{"runner":"one"}`))
	if err != nil {
		t.Fatal(err)
	}
	response, err := client.Do(request)
	if response != nil {
		response.Body.Close()
	}
	if err == nil {
		t.Fatal("transport-failed mutation unexpectedly succeeded")
	}
	if got := transport.calls.Load(); got != 1 {
		t.Fatalf("mutation transport attempts = %d, want exactly 1", got)
	}
}

func TestStrictHTTPClientDoesNotFollowMutationRedirects(t *testing.T) {
	for _, status := range []int{
		http.StatusMovedPermanently,
		http.StatusFound,
		http.StatusTemporaryRedirect,
		http.StatusPermanentRedirect,
	} {
		t.Run(http.StatusText(status), func(t *testing.T) {
			var originCalls atomic.Int32
			var redirectedCalls atomic.Int32
			redirected := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
				redirectedCalls.Add(1)
			}))
			defer redirected.Close()
			origin := httptest.NewServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
				originCalls.Add(1)
				if request.Method != http.MethodPost {
					t.Errorf("origin method = %s, want POST", request.Method)
				}
				response.Header().Set("Location", redirected.URL+"/replayed")
				response.WriteHeader(status)
			}))
			defer origin.Close()

			client := newStrictHTTPClient()
			client.Logger = nil
			request, err := retryablehttp.NewRequest(http.MethodPost, origin.URL+"/generatejitconfig", []byte(`{"runner":"one"}`))
			if err != nil {
				t.Fatal(err)
			}
			response, err := client.Do(request)
			if err != nil {
				t.Fatalf("redirect refusal should return the original response: %v", err)
			}
			defer response.Body.Close()
			if response.StatusCode != status {
				t.Fatalf("status = %d, want %d", response.StatusCode, status)
			}
			if got := originCalls.Load(); got != 1 {
				t.Fatalf("origin mutation requests = %d, want exactly 1", got)
			}
			if got := redirectedCalls.Load(); got != 0 {
				t.Fatalf("redirected mutation requests = %d, want 0", got)
			}
		})
	}
}
