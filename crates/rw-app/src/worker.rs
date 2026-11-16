//! Background regeneration thread. The UI never blocks on the kernel: edits
//! are sent here, intermediate requests are coalesced (only the newest part is
//! rebuilt), and results come back as plain data for rendering.

use rw_core::kernel::{MassProps, Mesh, Solid};
use rw_core::{FeatureStatus, Part, Regenerator, SolvedSketch};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;

pub enum Request {
    Regen(u64, Part),
    Export(String, PathBuf),
}

pub enum Response {
    Regen(Box<Rebuilt>),
    Exported(Result<PathBuf, String>),
}

pub struct Rebuilt {
    pub generation: u64,
    pub solved: Part,
    pub mesh: Option<Arc<Mesh>>,
    pub mass: Option<MassProps>,
    pub status: Vec<FeatureStatus>,
    pub sketches: Vec<SolvedSketch>,
    pub millis: f64,
    pub cache_hits: usize,
}

pub struct Worker {
    tx: Sender<Request>,
    pub rx: Receiver<Response>,
    pub busy: bool,
}

impl Worker {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Worker {
        let (tx, req_rx) = channel::<Request>();
        let (resp_tx, rx) = channel::<Response>();
        std::thread::Builder::new()
            .name("rustworks-regen".into())
            .spawn(move || {
                let mut regen = Regenerator::new();
                let mut body: Option<Solid> = None;
                while let Ok(mut req) = req_rx.recv() {
                    // Coalesce: skip to the newest regen request, but never drop exports.
                    loop {
                        match req_rx.try_recv() {
                            Ok(next @ Request::Regen(..)) if matches!(req, Request::Regen(..)) => req = next,
                            Ok(other) => {
                                handle(&mut regen, &mut body, req, &resp_tx);
                                req = other;
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }
                    handle(&mut regen, &mut body, req, &resp_tx);
                    repaint();
                }
            })
            .expect("spawn regen thread");
        Worker { tx, rx, busy: false }
    }

    pub fn send(&mut self, r: Request) {
        if matches!(r, Request::Regen(..)) {
            self.busy = true;
        }
        let _ = self.tx.send(r);
    }
}

fn handle(regen: &mut Regenerator, body: &mut Option<Solid>, req: Request, tx: &Sender<Response>) {
    match req {
        Request::Regen(generation, part) => {
            let out = regen.regenerate(&part);
            *body = out.body.clone();
            let _ = tx.send(Response::Regen(Box::new(Rebuilt {
                generation,
                solved: out.solved,
                mesh: out.mesh.map(Arc::new),
                mass: out.mass,
                status: out.status,
                sketches: out.sketches,
                millis: out.millis,
                cache_hits: out.cache_hits,
            })));
        }
        Request::Export(fmt, path) => {
            let r = (|| -> Result<PathBuf, String> {
                let b = body.as_ref().ok_or("nothing to export: the part has no body")?;
                match fmt.as_str() {
                    "step" => rw_core::kernel::write_step(b, &path).map_err(|e| e.to_string())?,
                    "stl" | "obj" => {
                        let tol = (rw_core::kernel::scale_of(b) / 4000.0).clamp(0.001, 0.2);
                        let mesh = rw_core::kernel::tessellate(b, tol, false);
                        let f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                        let mut w = std::io::BufWriter::new(f);
                        if fmt == "stl" { mesh.write_stl(&mut w) } else { mesh.write_obj(&mut w) }.map_err(|e| e.to_string())?;
                    }
                    _ => return Err(format!("unknown format {fmt}")),
                }
                Ok(path.clone())
            })();
            let _ = tx.send(Response::Exported(r));
        }
    }
}
