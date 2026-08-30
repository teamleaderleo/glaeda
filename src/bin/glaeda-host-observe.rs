#[cfg(not(target_os = "linux"))]
compile_error!("glaeda-host-observe requires Linux");

use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use glaeda::linux_host_observation::{
    DEFAULT_WATCHED_PORTS, LinuxHostObservation, LinuxHostObservationError, MAX_WATCHED_PORTS,
    ObservedCount, PressureSample, observe_linux_host,
};
use glaeda::process::ProcessExecutor;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-host-observe",
    about = "Observe bounded Linux machine handoff facts without mutation"
)]
struct Cli {
    /// Select human or JSON output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,

    /// Watch one TCP port instead of the bounded default development-port set.
    #[arg(long = "port", value_parser = clap::value_parser!(u16).range(1..))]
    ports: Vec<u16>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Serialize)]
struct LinuxHostObservationErrorReport<'a> {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    error: &'a LinuxHostObservationError,
}

impl<'a> LinuxHostObservationErrorReport<'a> {
    fn new(error: &'a LinuxHostObservationError) -> Self {
        Self {
            document_type: "glaeda-linux-host-observation-error",
            schema_version: 1,
            authority: "observation_only",
            error,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ports = if cli.ports.is_empty() {
        DEFAULT_WATCHED_PORTS
    } else {
        &cli.ports
    };
    if ports.len() > MAX_WATCHED_PORTS {
        return emit_error(
            cli.output,
            &LinuxHostObservationError {
                kind: glaeda::linux_host_observation::LinuxHostObservationErrorKind::InvalidRequest,
                code: "invalid_host_observation_request",
                problem: "host observation request is outside the bounded supported shape",
            },
        );
    }
    match observe_linux_host(ports, &ProcessExecutor) {
        Ok(report) => emit_report(cli.output, &report),
        Err(error) => emit_error(cli.output, &error),
    }
}

fn emit_report(output: OutputFormat, report: &LinuxHostObservation) -> ExitCode {
    match output {
        OutputFormat::Human => print!("{}", render_human(report)),
        OutputFormat::Json => match serde_json::to_string(report) {
            Ok(json) => println!("{json}"),
            Err(_) => {
                eprintln!("Linux host observation could not be encoded");
                return ExitCode::from(2);
            }
        },
    }
    ExitCode::SUCCESS
}

fn emit_error(output: OutputFormat, error: &LinuxHostObservationError) -> ExitCode {
    let report = LinuxHostObservationErrorReport::new(error);
    match output {
        OutputFormat::Human => eprintln!(
            "Linux host observation unavailable: code={} problem={}",
            error.code, error.problem
        ),
        OutputFormat::Json => match serde_json::to_string(&report) {
            Ok(json) => eprintln!("{json}"),
            Err(_) => eprintln!("Linux host observation error could not be encoded"),
        },
    }
    ExitCode::from(2)
}

fn render_human(report: &LinuxHostObservation) -> String {
    let cpu = report.cpu();
    let memory = report.memory();
    let pressure = report.pressure();
    let services = report.services();
    let ports = report
        .watched_ports()
        .iter()
        .map(|entry| {
            format!(
                "{}={}",
                entry.port,
                if entry.listening {
                    "listening"
                } else {
                    "absent"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "Linux host observation: authority=observation_only observed_at={}\n",
            "cpu: logical={} load={} runnable={}/{} pressure={}\n",
            "memory: total={} available={} swap_used={} swap_total={} pressure={}\n",
            "io: pressure={}\n",
            "failed units: system={} user={}\n",
            "watched ports: {}\n"
        ),
        report.observed_at_unix_millis(),
        cpu.logical_cpus,
        format_micros(cpu.load_1m_micros),
        cpu.runnable_entities,
        cpu.total_entities,
        render_pressure(pressure.cpu),
        memory.total_bytes,
        memory.available_bytes,
        memory.swap_used_bytes,
        memory.swap_total_bytes,
        render_pressure(pressure.memory),
        render_pressure(pressure.io),
        render_count(services.system),
        render_count(services.user),
        ports,
    )
}

fn render_pressure(sample: PressureSample) -> String {
    format!(
        "avg10:{} total:{}",
        format_micros(u64::from(sample.avg10_micros)),
        sample.total_micros
    )
}

fn render_count(count: ObservedCount) -> String {
    match count {
        ObservedCount::Known { count } => count.to_string(),
        ObservedCount::Unavailable => "unavailable".to_owned(),
    }
}

fn format_micros(value: u64) -> String {
    format!("{}.{:06}", value / 1_000_000, value % 1_000_000)
}
