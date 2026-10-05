// Photo transfers: each ASIC sends a photo's records over virtual channel 1 of its CSI-2
// link, beside the preview, into its own CAMSS RDI (/dev/video1-3). Those streams run for as
// long as the app does: stopping an RDI that is not receiving times out in CAMSS and leaves
// it unusable. Records arrive in the order they were asked for; each goes into the oldest
// photo still waiting for that ASIC's records.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

// what CAMSS stores of a record: 1408 x 9219 RAW10 (1760 bytes a line)
const FRAME: usize = 1760 * 9219;

pub struct Photo {
    pub dir: PathBuf,
    records: [u16; 3],
    left: [u16; 3],
    files: [Option<File>; 3],
}

impl Photo {
    pub fn new(dir: PathBuf, records: [u16; 3]) -> std::io::Result<Photo> {
        std::fs::create_dir_all(&dir)?;
        let mut files = [None, None, None];
        for a in 0..3 {
            if records[a] > 0 {
                files[a] = Some(File::create(dir.join(format!("asic{}.raw", a + 1)))?);
            }
        }
        Ok(Photo { dir, records, left: records, files })
    }

    // records ASIC @a has sent so far
    pub fn received(&self, a: usize) -> u16 {
        self.records[a] - self.left[a]
    }
}

pub type Queue = Arc<Mutex<VecDeque<Photo>>>;

pub struct Transfers {
    pub queue: Queue,
    streams: Vec<Child>,
}

impl Transfers {
    // links and formats the paths, starts the streams; @done gets each photo's directory
    // once all its records are in
    // done: a photo's directory once all its records are in, or the directory and what failed
    pub fn start(done: mpsc::Sender<Result<PathBuf, (PathBuf, String)>>) -> Result<Transfers, String> {
        let o = Command::new("l16-shoot").arg("setup").output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let mut streams = Vec::new();
        for a in 0..3usize {
            let mut cmd = Command::new("v4l2-ctl");
            cmd.args(["-d", &format!("/dev/video{}", a + 1)])
                // eight buffers: a photo's records (six per ASIC) all fit, so the RDI never
                // runs dry mid-photo (CAMSS's re-arm of an idle RDI loses what follows)
                .args(["--stream-mmap=8", "--stream-to=-"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            // if the app dies without closing (a SIGKILL), the stream goes with it rather than
            // holding the RDI for the next start
            #[cfg(target_os = "linux")]
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let mut out = child.stdout.take().unwrap();
            let queue = queue.clone();
            let done = done.clone();
            thread::spawn(move || {
                let mut buf = vec![0u8; FRAME];
                while out.read_exact(&mut buf).is_ok() {
                    let mut q = queue.lock().unwrap();
                    let Some(i) = q.iter().position(|p| p.left[a] > 0) else {
                        eprintln!("l16-camera2: ASIC{} record with no photo waiting, dropped", a + 1);
                        continue;
                    };
                    let ph = &mut q[i];
                    let r = ph.files[a].as_mut().map(|f| f.write_all(&buf));
                    ph.left[a] -= 1;
                    if let Some(Err(e)) = r {
                        let dir = q.remove(i).unwrap().dir;
                        let msg = format!("{}: {e}", dir.display());
                        let _ = done.send(Err((dir, msg)));
                    } else if ph.left.iter().all(|&n| n == 0) {
                        let _ = done.send(Ok(q.remove(i).unwrap().dir));
                    }
                }
            });
            streams.push(child);
        }
        eprintln!("l16-camera2: transfers started");
        Ok(Transfers { queue, streams })
    }

    pub fn stop(&mut self) {
        for c in &mut self.streams {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.streams.clear();
        // ASIC1's path leaves the preview's CSID: with nothing streaming, starting the preview
        // validates every enabled link off csid0, and this one's format (the records') is not
        // the preview's (EPIPE, the preview black until a reboot). setup links it again
        let r = Command::new("media-ctl")
            .args(["-d", "/dev/media0", "-l", "\"msm_csid0\":1->\"msm_ispif1\":0[4]"])
            .output();
        match r {
            Ok(o) if o.status.success() => eprintln!("l16-camera2: transfers stopped, ASIC1 link off"),
            Ok(o) => eprintln!("l16-camera2: transfers stopped, ASIC1 link off failed: {}",
                               String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => eprintln!("l16-camera2: transfers stopped, media-ctl: {e}"),
        }
    }
}
