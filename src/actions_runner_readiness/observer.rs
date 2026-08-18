#[derive(Clone, PartialEq, Eq)]
pub struct ActionsRunnerReadinessAdapter {
    limactl_program: PathBuf,
}

impl ActionsRunnerReadinessAdapter {
    /// Bind one reviewed absolute `limactl` executable.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the executable path is canonical and absolute.
    pub fn new(limactl_program: impl Into<PathBuf>) -> Result<Self, ActionsRunnerReadinessFailure> {
        let limactl_program = validate_private_path(limactl_program.into(), false)?;
        Ok(Self { limactl_program })
    }

    /// Observe one configured official Actions runner without registration or lifecycle mutation.
    ///
    /// # Errors
    ///
    /// Returns bounded typed failures for source, command, configured identity, process identity,
    /// output, or intra-observation drift problems.
    pub fn observe(
        &self,
        request: &ActionsRunnerReadinessRequest,
        source: &LimaInstanceObservationReport,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<ActionsRunnerReadinessObservation, ActionsRunnerReadinessFailure> {
        let mut evidence = ActionsRunnerReadinessPrivateEvidence::default();
        let result = self.observe_inner(request, source, executor, clock, &mut evidence);
        match result {
            Ok(public) => Ok(ActionsRunnerReadinessObservation {
                public,
                private_evidence: evidence,
            }),
            Err(problem) => Err(ActionsRunnerReadinessFailure::from_problem(
                problem, evidence,
            )),
        }
    }

    fn observe_inner(
        &self,
        request: &ActionsRunnerReadinessRequest,
        source: &LimaInstanceObservationReport,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
    ) -> Result<ActionsRunnerReadinessReport, ObservationProblem> {
        if source.instance != request.instance {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::SourceInstanceMismatch,
                ActionsRunnerReadinessPhase::SourceObservation,
                "the runner-readiness request does not match the exact Lima instance observation",
            ));
        }
        let started_at = clock.unix_seconds().map_err(|_| clock_problem())?;
        let source_freshness = source.timing.freshness_at(started_at);
        if source_freshness != LimaObservationFreshness::Fresh {
            return Ok(report(
                request,
                ActionsRunnerReadinessState::Stale,
                None,
                timing(started_at, started_at, source, source_freshness)?,
            ));
        }

        match source.configured.runtime_state {
            LimaRuntimeState::Stopped => {
                return Ok(report(
                    request,
                    ActionsRunnerReadinessState::Offline,
                    None,
                    timing(
                        started_at,
                        started_at,
                        source,
                        LimaObservationFreshness::Fresh,
                    )?,
                ));
            }
            LimaRuntimeState::Uninitialized | LimaRuntimeState::Installing => {
                return Ok(report(
                    request,
                    ActionsRunnerReadinessState::Starting,
                    None,
                    timing(
                        started_at,
                        started_at,
                        source,
                        LimaObservationFreshness::Fresh,
                    )?,
                ));
            }
            LimaRuntimeState::Broken => {
                return Err(ObservationProblem::new(
                    ActionsRunnerReadinessRefusalCode::SourceUnavailable,
                    ActionsRunnerReadinessPhase::SourceObservation,
                    "the exact Lima source observation reports an unavailable instance",
                ));
            }
            LimaRuntimeState::Running => {}
        }
        if !matches!(&source.guest, LimaGuestObservation::Observed(_)) {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::SourceGuestMismatch,
                ActionsRunnerReadinessPhase::SourceObservation,
                "the running Lima source observation lacks matching guest evidence",
            ));
        }

        let initial_identity = self.observe_identity(request, executor, evidence)?;
        let initial_draining = self.observe_drain_marker(request, executor, evidence)?;
        let initial_processes = self.observe_processes(request, executor, evidence, false)?;

        let final_processes = self.observe_processes(request, executor, evidence, true)?;
        if final_processes != initial_processes {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessDrift,
                ActionsRunnerReadinessPhase::FinalObservation,
                "the official runner process identity changed during observation",
            ));
        }
        let final_identity = self.observe_identity(request, executor, evidence)?;
        if final_identity != initial_identity {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::IdentityDrift,
                ActionsRunnerReadinessPhase::FinalObservation,
                "the configured official runner identity changed during observation",
            ));
        }
        let final_draining = self.observe_drain_marker(request, executor, evidence)?;
        if final_draining != initial_draining {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::DrainStateDrift,
                ActionsRunnerReadinessPhase::FinalObservation,
                "the reviewed runner drain marker changed during observation",
            ));
        }

        let state = classify_state(&initial_processes, initial_draining)?;
        let observed_at = clock.unix_seconds().map_err(|_| clock_problem())?;
        let freshness = source.timing.freshness_at(observed_at);
        if freshness != LimaObservationFreshness::Fresh {
            return Ok(report(
                request,
                ActionsRunnerReadinessState::Stale,
                None,
                timing(started_at, observed_at, source, freshness)?,
            ));
        }
        let configured_identity = Some(ActionsRunnerConfiguredIdentity {
            runner_name: request.runner_name.clone(),
            configuration_digest: initial_identity.configuration_digest,
            runner_root: initial_identity.root,
        });
        Ok(report(
            request,
            state,
            configured_identity,
            timing(started_at, observed_at, source, freshness)?,
        ))
    }

    fn observe_identity(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
    ) -> Result<IdentitySnapshot, ObservationProblem> {
        let root = parse_filesystem_identity(
            &self.run_success(
                request,
                executor,
                evidence,
                ActionsRunnerReadinessPhase::RunnerRootIdentity,
                self.guest_private_path_command(
                    request,
                    GUEST_STAT,
                    ["-Lc", "%d:%i", "--"],
                    &request.runner_root,
                ),
            )?,
            ActionsRunnerReadinessPhase::RunnerRootIdentity,
        )?;
        let configuration_digest = parse_private_sha256(&self.run_success(
            request,
            executor,
            evidence,
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            self.guest_private_path_command(
                request,
                GUEST_SHA256SUM,
                ["--"],
                &request.configuration_path,
            ),
        )?)?;
        if configuration_digest != request.expected_configuration_digest {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ConfigurationIdentityMismatch,
                ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
                "the official runner configuration digest differs from the reviewed identity",
            ));
        }
        let listener_digest = self.observe_private_digest(request, executor, evidence, &request.listener_path)?;
        let worker_digest = self.observe_private_digest(request, executor, evidence, &request.worker_path)?;
        let credentials_digest =
            self.observe_private_digest(request, executor, evidence, &request.credentials_path)?;
        let credentials_rsa_parameters_digest = self.observe_private_digest(
            request,
            executor,
            evidence,
            &request.credentials_rsa_parameters_path,
        )?;
        if listener_digest != request.expected_listener_digest
            || worker_digest != request.expected_worker_digest
            || credentials_digest != request.expected_credentials_digest
            || credentials_rsa_parameters_digest
                != request.expected_credentials_rsa_parameters_digest
        {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::InstallationIdentityMismatch,
                ActionsRunnerReadinessPhase::RunnerInstallationIdentity,
                "the official runner installation differs from the reviewed identity",
            ));
        }
        Ok(IdentitySnapshot {
            root,
            configuration_digest,
            listener_digest,
            worker_digest,
            credentials_digest,
            credentials_rsa_parameters_digest,
        })
    }

    fn observe_private_digest(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        path: &Path,
    ) -> Result<Sha256Digest, ObservationProblem> {
        parse_private_sha256(&self.run_success(
            request,
            executor,
            evidence,
            ActionsRunnerReadinessPhase::RunnerInstallationIdentity,
            self.guest_private_path_command(request, GUEST_SHA256SUM, ["--"], path),
        )?)
    }

    fn observe_drain_marker(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
    ) -> Result<bool, ObservationProblem> {
        let record = self.execute_record(
            executor,
            evidence,
            ActionsRunnerReadinessPhase::DrainMarker,
            self.guest_private_path_command(
                request,
                GUEST_TEST,
                ["-e"],
                &request.drain_marker_path,
            ),
        )?;
        if !record.stdout.is_empty() || !record.stderr.is_empty() {
            return Err(malformed_identity(
                ActionsRunnerReadinessPhase::DrainMarker,
                "the reviewed drain marker probe returned unexpected output",
            ));
        }
        match (record.status, record.success) {
            (Some(0), true) => Ok(true),
            (Some(1), false) => Ok(false),
            _ => Err(command_failed(
                ActionsRunnerReadinessPhase::DrainMarker,
                "the reviewed drain marker probe did not complete cleanly",
            )),
        }
    }

    fn observe_processes(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        final_observation: bool,
    ) -> Result<ProcessSnapshot, ObservationProblem> {
        let listener_phase = if final_observation {
            ActionsRunnerReadinessPhase::FinalObservation
        } else {
            ActionsRunnerReadinessPhase::ListenerDiscovery
        };
        let worker_phase = if final_observation {
            ActionsRunnerReadinessPhase::FinalObservation
        } else {
            ActionsRunnerReadinessPhase::WorkerDiscovery
        };
        let listener_pids = self.observe_named_processes(
            request,
            executor,
            evidence,
            listener_phase,
            LISTENER_NAME,
        )?;
        let worker_pids =
            self.observe_named_processes(request, executor, evidence, worker_phase, WORKER_NAME)?;
        if listener_pids.len() > 1 {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::AmbiguousListener,
                listener_phase,
                "more than one official runner listener matched the reviewed identity",
            ));
        }
        if worker_pids.len() > 1 {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::AmbiguousWorker,
                worker_phase,
                "more than one official runner worker matched the reviewed identity",
            ));
        }
        let listener = listener_pids
            .first()
            .copied()
            .map(|pid| {
                self.verify_process(
                    request,
                    executor,
                    evidence,
                    ActionsRunnerReadinessPhase::ListenerIdentity,
                    pid,
                    (&request.listener_path, &request.expected_listener_digest),
                )
            })
            .transpose()?;
        let worker = worker_pids
            .first()
            .copied()
            .map(|pid| {
                self.verify_process(
                    request,
                    executor,
                    evidence,
                    ActionsRunnerReadinessPhase::WorkerIdentity,
                    pid,
                    (&request.worker_path, &request.expected_worker_digest),
                )
            })
            .transpose()?;
        Ok(ProcessSnapshot { listener, worker })
    }

    fn observe_named_processes(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        process_name: &str,
    ) -> Result<Vec<u32>, ObservationProblem> {
        let record = self.execute_record(
            executor,
            evidence,
            phase,
            self.guest_plain_command(request, GUEST_PGREP, ["-x", process_name]),
        )?;
        match (record.status, record.success) {
            (Some(1), false) if record.stdout.is_empty() && record.stderr.is_empty() => {
                return Ok(Vec::new());
            }
            (Some(0), true) if record.stderr.is_empty() => {}
            _ => {
                return Err(command_failed(
                    phase,
                    "the exact official runner process query did not complete cleanly",
                ));
            }
        }
        parse_pid_lines(&record.stdout, phase)
    }

    fn verify_process(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        pid: u32,
        expected_executable: (&Path, &Sha256Digest),
    ) -> Result<ProcessIdentity, ObservationProblem> {
        let proc_root = format!("/proc/{pid}");
        let proc_exe = format!("{proc_root}/exe");
        let proc_cwd = format!("{proc_root}/cwd");
        let executable_output = self.run_success(
            request,
            executor,
            evidence,
            phase,
            self.guest_plain_command(request, GUEST_READLINK, ["-e", "--", proc_exe.as_str()]),
        )?;
        let executable = parse_single_line(&executable_output, phase)?;
        if Path::new(executable) != expected_executable.0 {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch,
                phase,
                "the official runner process executable differs from the reviewed identity",
            ));
        }
        let executable_digest = parse_private_sha256(&self.run_success(
            request,
            executor,
            evidence,
            phase,
            self.guest_private_path_command(request, GUEST_SHA256SUM, ["--"], Path::new(&proc_exe)),
        )?)?;
        if &executable_digest != expected_executable.1 {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch,
                phase,
                "the official runner process executable digest differs from the reviewed identity",
            ));
        }
        let cwd_output = self.run_success(
            request,
            executor,
            evidence,
            phase,
            self.guest_plain_command(request, GUEST_READLINK, ["-e", "--", proc_cwd.as_str()]),
        )?;
        let cwd = parse_single_line(&cwd_output, phase)?;
        if Path::new(cwd) != request.runner_root {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch,
                phase,
                "the official runner process working directory differs from the reviewed root",
            ));
        }
        let proc_object = parse_filesystem_identity(
            &self.run_success(
                request,
                executor,
                evidence,
                phase,
                self.guest_plain_command(
                    request,
                    GUEST_STAT,
                    ["-Lc", "%d:%i", "--", proc_root.as_str()],
                ),
            )?,
            phase,
        )?;
        Ok(ProcessIdentity { pid, proc_object })
    }

    fn run_success(
        &self,
        _request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        command: CommandSpec,
    ) -> Result<String, ObservationProblem> {
        let record = self.execute_record(executor, evidence, phase, command)?;
        if record.status != Some(0) || !record.success || !record.stderr.is_empty() {
            return Err(command_failed(
                phase,
                "the reviewed official runner observation command did not complete cleanly",
            ));
        }
        Ok(record.stdout)
    }

    fn execute_record(
        &self,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        command: CommandSpec,
    ) -> Result<ExecutionRecord, ObservationProblem> {
        let record = executor.execute(&command).map_err(|_| {
            command_failed(
                phase,
                "the reviewed official runner observation command could not be executed",
            )
        })?;
        evidence.commands.push(ActionsRunnerPrivateCommandEvidence {
            phase,
            record: record.clone(),
        });
        if record.argv != command.displayed_argv()
            || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::CommandIdentityMismatch,
                phase,
                "the subprocess record does not match the reviewed runner observation command",
            ));
        }
        if record.stdout.len() > MAX_ACTIONS_RUNNER_OUTPUT_BYTES
            || record.stderr.len() > MAX_ACTIONS_RUNNER_OUTPUT_BYTES
        {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::UnboundedOutput,
                phase,
                "the official runner observation output exceeded the reviewed bound",
            ));
        }
        if record
            .stdout
            .chars()
            .chain(record.stderr.chars())
            .any(|character| matches!(character, '\0' | '\u{fffd}'))
        {
            return Err(malformed_identity(
                phase,
                "the official runner observation returned malformed text evidence",
            ));
        }
        Ok(record)
    }

    fn base_command(&self, request: &ActionsRunnerReadinessRequest) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .environment("HOME", LIMACTL_SAFE_HOME)
            .environment("LIMA_HOME", exact_path(&request.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
    }

    fn guest_plain_command<const N: usize>(
        &self,
        request: &ActionsRunnerReadinessRequest,
        program: &str,
        arguments: [&str; N],
    ) -> CommandSpec {
        let mut command = self
            .base_command(request)
            .argument("--tty=false")
            .argument("shell")
            .argument(request.instance.as_str())
            .argument("--")
            .argument(program);
        for argument in arguments {
            command = command.argument(argument);
        }
        command
    }

    fn guest_private_path_command<const N: usize>(
        &self,
        request: &ActionsRunnerReadinessRequest,
        program: &str,
        arguments: [&str; N],
        path: &Path,
    ) -> CommandSpec {
        let mut command = self.guest_plain_command(request, program, arguments);
        command = command.secret_argument(exact_path(path));
        command
    }
}

impl fmt::Debug for ActionsRunnerReadinessAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerReadinessAdapter")
            .field("limactl_program", &"<reviewed-absolute-limactl>")
            .finish()
    }
}
