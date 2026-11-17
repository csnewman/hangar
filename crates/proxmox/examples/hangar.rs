//! Makes, upgrades and removes a Hangar of one's own on a Proxmox from the
//! command line, as the app does (`hangar_proxmox::hangar`):
//!
//!   hangar probe URL
//!   hangar choices URL TOKEN_FILE FINGERPRINT
//!   hangar make URL TOKEN_FILE FINGERPRINT DIR node=… disks=… isos=… network=… pool=… prefix=… [admin=… cores=… memory=… data=… release=IMAGE]
//!   hangar upgrade URL TOKEN_FILE FINGERPRINT DIR [release=IMAGE]
//!   hangar remove URL TOKEN_FILE FINGERPRINT DIR
//!
//! TOKEN_FILE holds the Proxmox API token (USER@REALM!ID=SECRET). make keeps
//! what it made in DIR: made.json, and the Hangar's first access token in
//! owner-token (0600), neither printed.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use hangar_proxmox::hangar::{self, Made, Plan, Proxmox, RELEASE_ISO_IMAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hangar: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let e = |e: std::io::Error| e.to_string();
    let kv: HashMap<&str, &str> = args.iter().filter_map(|a| a.split_once('=')).collect();
    let get = |k: &str, d: &'static str| kv.get(k).copied().unwrap_or(d).to_string();
    let connect = || -> Result<Proxmox, String> {
        let token = std::fs::read_to_string(args.get(2).ok_or("no TOKEN_FILE")?)
            .map_err(|e| format!("the token file: {e}"))?;
        Proxmox::new(&args[1], &token, args.get(3).ok_or("no FINGERPRINT")?).map_err(e)
    };
    match args.first().map(String::as_str) {
        Some("probe") => {
            let fp = hangar::certificate(args.get(1).ok_or("no URL")?)
                .await
                .map_err(e)?;
            println!("{}", hangar::show_fingerprint(&fp));
        }
        Some("choices") => {
            let c = connect()?.choices().await.map_err(e)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&c).map_err(|e| e.to_string())?
            );
        }
        Some("make") => {
            let pve = connect()?;
            let dir = Path::new(args.get(4).ok_or("no DIR")?);
            let plan = Plan {
                node: get("node", ""),
                disks: get("disks", ""),
                isos: get("isos", ""),
                network: get("network", "vmbr0"),
                pool: get("pool", ""),
                prefix: get("prefix", ""),
                admin: get("admin", "owner"),
                cores: get("cores", "2").parse().map_err(|_| "cores")?,
                memory_mib: get("memory", "2048").parse().map_err(|_| "memory")?,
                data_gib: get("data", "64").parse().map_err(|_| "data")?,
            };
            let release = fetch(dir, &get("release", ""))?;
            let t0 = std::time::Instant::now();
            let made = pve
                .make(&plan, &release, &|s| {
                    eprintln!("{:6.1} s  {s:?}", t0.elapsed().as_secs_f64());
                })
                .await
                .map_err(e)?;
            keep(dir, &made)?;
            println!(
                "made VM {} at {} in {:.1} s",
                made.vmid,
                made.url,
                t0.elapsed().as_secs_f64()
            );
        }
        Some("upgrade") => {
            let dir = Path::new(args.get(4).ok_or("no DIR")?);
            let made = kept(dir)?;
            let release = fetch(dir, &get("release", ""))?;
            connect()?.upgrade(&made, &release).await.map_err(e)?;
            println!("moved VM {} to release {}", made.vmid, release.version);
        }
        Some("remove") => {
            let dir = Path::new(args.get(4).ok_or("no DIR")?);
            let made = kept(dir)?;
            connect()?.remove(&made).await.map_err(e)?;
            println!("removed VM {} and all named {}-", made.vmid, made.prefix);
        }
        _ => return Err("probe, choices, make, upgrade or remove: see the source's head".into()),
    }
    Ok(())
}

/// The release the image names (the latest published, unless given).
fn fetch(dir: &Path, image: &str) -> Result<hangar::Release, String> {
    let image = if image.is_empty() {
        format!("{RELEASE_ISO_IMAGE}:latest")
    } else {
        image.to_string()
    };
    let r = hangar::fetch_release(&image, &dir.join("release")).map_err(|e| e.to_string())?;
    eprintln!("release {} from {image}", r.version);
    Ok(r)
}

fn keep(dir: &Path, made: &Made) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join("owner-token"))
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, made.token.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("made.json"),
        serde_json::to_vec_pretty(made).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn kept(dir: &Path) -> Result<Made, String> {
    let b = std::fs::read(dir.join("made.json")).map_err(|e| e.to_string())?;
    let mut m: Made = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    m.token = std::fs::read_to_string(dir.join("owner-token")).unwrap_or_default();
    Ok(m)
}
