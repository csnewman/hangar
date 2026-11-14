//! The worker's commands besides running: checking that this machine can
//! run environments, and writing an EROFS image by hand.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::process::ExitCode;

use hangar_vm::host::{self, Monitor};

/// Prints what this machine offers environments and what is missing:
/// its capabilities, its monitor, and on Linux under Cloud Hypervisor,
/// whether shared memory can take huge pages.
pub fn doctor() -> ExitCode {
    let caps = match host::detect("") {
        Ok(c) => c,
        Err(e) => {
            println!(
                "host        {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            eprintln!("hangar-worker: {e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", caps.summary());
    if caps.monitor == Monitor::Krun {
        match hangar_vm::krun::find() {
            Ok(bin) => println!("hangar-krun {}", bin.display()),
            Err(e) => println!("hangar-krun not available: {e}"),
        }
        return ExitCode::SUCCESS;
    }
    match hangar_vm::ch::find() {
        Ok(bin) => println!("cloud-hyp   {}", bin.display()),
        Err(e) => println!("cloud-hyp   not available: {e}"),
    }
    match hangar_vm::hugepages::check() {
        Ok(()) => println!("huge pages  yes, shared memory is eligible"),
        // The explanation is long, and worth printing only when it applies.
        Err(e) => println!("huge pages  NO\n\n{e}"),
    }
    ExitCode::SUCCESS
}

const EROFS_USAGE: &str =
    "usage: hangar-worker erofs -o IMAGE DIR\n       hangar-worker erofs -o IMAGE -tar < ARCHIVE";

/// Writes an EROFS image, as the worker makes each image's, of a directory
/// or of a tar archive on standard input.
pub fn erofs(args: &[String]) -> ExitCode {
    let mut out = None;
    let mut tar = false;
    let mut dir = None;
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--o" => out = args.next(),
            "-tar" | "--tar" => tar = true,
            "-h" | "-help" | "--help" => {
                println!("{EROFS_USAGE}");
                return ExitCode::SUCCESS;
            }
            s if !s.starts_with('-') && dir.is_none() => dir = Some(a),
            _ => return usage(),
        }
    }
    let Some(out) = out else {
        return usage();
    };
    if tar == dir.is_some() {
        return usage();
    }
    let out = Path::new(out);
    match write_erofs(out, dir.map(Path::new)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = std::fs::remove_file(out);
            eprintln!("hangar-worker: {}: {e}", out.display());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{EROFS_USAGE}");
    ExitCode::from(2)
}

/// Writes dir, or else the tar archive on standard input, to out.
fn write_erofs(out: &Path, dir: Option<&Path>) -> io::Result<()> {
    let mut w = hangar_erofs::Writer::new(File::create(out)?);
    match dir {
        Some(d) => w.add_dir(d)?,
        None => hangar_images::Merge::default().layer(
            BufReader::with_capacity(1 << 20, io::stdin().lock()),
            &mut w,
        )?,
    }
    w.close()?;
    let size = w.size();
    w.into_inner().set_len(size)
}
