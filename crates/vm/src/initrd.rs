//! The initramfs an environment boots.

use std::io::{self, Write};
use std::path::Path;

/// Writes the initramfs an environment boots: the agent (crates/agent), as
/// its init, and nothing else. The agent assembles the root from the base
/// and the writable disk, installs itself there, and hands over to the
/// image's own init (crates/agent/src/init.rs). So an image is a root
/// filesystem and nothing else, and every environment runs the agent of the
/// worker that boots it.
pub fn write_initrd(agent: &Path, path: &Path) -> io::Result<()> {
    let bin = std::fs::read(agent)
        .map_err(|e| io::Error::new(e.kind(), format!("reading the agent: {e}")))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = std::path::PathBuf::from(tmp);
    let written = (|| {
        let mut out = io::BufWriter::new(std::fs::File::create(&tmp)?);
        write_cpio(
            &mut out,
            &[Entry {
                name: "init",
                mode: 0o100755,
                data: &bin,
            }],
        )?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)
}

struct Entry<'a> {
    name: &'a str,
    mode: u32,
    data: &'a [u8],
}

/// Writes entries as a "newc" cpio archive, the format the kernel unpacks
/// an initramfs from.
fn write_cpio(w: &mut impl Write, entries: &[Entry<'_>]) -> io::Result<()> {
    let pad = |n: usize| vec![0u8; (4 - n % 4) % 4];
    let mut write = |ino: usize, e: &Entry<'_>| -> io::Result<()> {
        let name = format!("{}\0", e.name);
        let nlink = if e.mode & 0o040000 != 0 { 2 } else { 1 };
        let fields = [
            ino,
            e.mode as usize,
            0,
            0,
            nlink,
            0,
            e.data.len(),
            0,
            0,
            0,
            0,
            name.len(),
            0,
        ];
        let mut hdr = String::from("070701");
        for f in fields {
            hdr.push_str(&format!("{f:08X}"));
        }
        w.write_all(hdr.as_bytes())?;
        w.write_all(name.as_bytes())?;
        w.write_all(&pad(hdr.len() + name.len()))?;
        w.write_all(e.data)?;
        w.write_all(&pad(e.data.len()))
    };
    for (i, e) in entries.iter().enumerate() {
        write(i + 1, e)?;
    }
    write(
        entries.len() + 1,
        &Entry {
            name: "TRAILER!!!",
            mode: 0,
            data: &[],
        },
    )
}
