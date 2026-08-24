#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[path = "project_disk_physical_receipt/receipt/mod.rs"]
mod receipt;

#[cfg(target_os = "macos")]
#[path = "project_disk_physical_receipt/capture/mod.rs"]
mod capture;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("project-disk physical receipt capture must run on the operator Mac");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::run()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::PathBuf;
    use std::time::Duration;

    use clap::Parser;

    use crate::capture::{
        ProjectDiskPhysicalCaptureRequest, capture_project_disk_physical_receipt,
    };
    use smolrunner::process::ProcessExecutor;

    #[derive(Debug, Parser)]
    #[command(about = "Capture a private observation-only project-disk physical receipt")]
    struct Args {
        #[arg(long)]
        repo_commit: String,
        #[arg(long)]
        lima_home: PathBuf,
        #[arg(long)]
        disk_directory: PathBuf,
        #[arg(long)]
        disk_name: String,
        #[arg(long)]
        resident_sandbox_instance: String,
        #[arg(long)]
        guest_project_mount: PathBuf,
        #[arg(long)]
        guest_cache_path: PathBuf,
        #[arg(long)]
        limactl: PathBuf,
        #[arg(long)]
        project_identity: String,
        #[arg(long)]
        project_disk_id: String,
        #[arg(long)]
        project_disk_generation: u64,
        #[arg(long)]
        project_disk_revision: u64,
        #[arg(long)]
        attachment_generation: u64,
        #[arg(long)]
        resident_sandbox_id: String,
        #[arg(long)]
        resident_sandbox_generation: u64,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
        #[arg(long)]
        output: PathBuf,
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args = Args::parse();
        let request = ProjectDiskPhysicalCaptureRequest::new(
            args.repo_commit,
            args.lima_home,
            args.disk_directory,
            args.disk_name,
            args.resident_sandbox_instance,
            args.guest_project_mount,
            args.guest_cache_path,
            args.limactl,
            args.project_identity,
            args.project_disk_id,
            args.project_disk_generation,
            args.project_disk_revision,
            args.attachment_generation,
            args.resident_sandbox_id,
            args.resident_sandbox_generation,
            Duration::from_secs(args.timeout_seconds),
        )?;
        let receipt = capture_project_disk_physical_receipt(&request, &ProcessExecutor)?;
        let bytes = receipt.encode_private_json_pretty()?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&args.output)?;
        output.write_all(&bytes)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        println!("private project-disk physical receipt captured");
        Ok(())
    }
}
