//! Blender worker driver. Spawns Blender as a subprocess and speaks the
//! JSON-line protocol used by `workers/blender/aec_blender_worker.py`.
//!
//! In Phase 1 the driver owns:
//!   * the request/response envelopes (typed structs below),
//!   * a request id allocator,
//!   * the worker lifecycle state machine.
//!
//! Spawning the actual subprocess lives in `aec_bridge` (which controls
//! OS-level resources).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::preset::RenderPreset;
use crate::scene::RenderScene;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("worker not running")]
    NotRunning,
    #[error("worker handshake failed: {0}")]
    Handshake(String),
    #[error("worker reported error: {0}")]
    Reported(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Stopped,
    Starting,
    Ready,
    Busy,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlenderRequest {
    /// First message sent. Worker replies with `Ready`.
    Handshake { client_version: String },
    /// Send a scene + preset and render with EEVEE.
    EeveeRender {
        scene: Box<RenderScene>,
        preset: Box<RenderPreset>,
        output_path: String,
    },
    /// Send a scene + preset and render with Cycles.
    CyclesRender {
        scene: Box<RenderScene>,
        preset: Box<RenderPreset>,
        output_path: String,
    },
    /// Cycles equirectangular 360° panorama render.
    PanoramaRender {
        scene: Box<RenderScene>,
        preset: Box<RenderPreset>,
        output_path: String,
    },
    /// Cycles walkthrough render (image sequence) with a camera path.
    WalkthroughRender {
        scene: Box<RenderScene>,
        preset: Box<RenderPreset>,
        camera_path: crate::cameras::CameraPath,
        output_dir: String,
        frame_start: u32,
        frame_end: u32,
    },
    /// Stitch a directory of walkthrough frames into an MP4 video.
    /// When the worker has no FFmpeg available it returns
    /// [`WalkthroughOutput::ImageSequence`] pointing at `input_dir`.
    StitchWalkthrough {
        input_dir: String,
        output_path: String,
        /// Frames per second of the resulting MP4. 24 is the default
        /// the renderer pipeline uses.
        fps: u32,
        /// Glob pattern for the frame files; defaults to
        /// `frame_%05d.png` when None.
        frame_pattern: Option<String>,
    },
    /// Graceful shutdown.
    Shutdown,
}

/// Output of a [`BlenderRequest::StitchWalkthrough`] request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WalkthroughOutput {
    /// FFmpeg produced an MP4 at this path.
    Video { path: String },
    /// FFmpeg was unavailable — the worker left the still frames in
    /// this directory so the user can stitch them externally.
    ImageSequence { dir: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlenderResponse {
    Ready {
        blender_version: String,
        worker_version: String,
    },
    Progress {
        job_id: String,
        progress: f32,
    },
    RenderCompleted {
        job_id: String,
        output_path: String,
    },
    /// Reply to `BlenderRequest::StitchWalkthrough`.
    WalkthroughStitched {
        job_id: String,
        output: WalkthroughOutput,
    },
    Error {
        message: String,
    },
}

pub struct BlenderWorker {
    state: WorkerState,
    next_request_id: u64,
}

impl Default for BlenderWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl BlenderWorker {
    pub fn new() -> Self {
        Self {
            state: WorkerState::Stopped,
            next_request_id: 0,
        }
    }

    pub fn state(&self) -> WorkerState {
        self.state
    }

    pub fn begin_start(&mut self) {
        self.state = WorkerState::Starting;
    }

    pub fn mark_ready(&mut self) {
        self.state = WorkerState::Ready;
    }

    pub fn mark_busy(&mut self) {
        self.state = WorkerState::Busy;
    }

    pub fn mark_failed(&mut self) {
        self.state = WorkerState::Failed;
    }

    pub fn stop(&mut self) {
        self.state = WorkerState::Stopped;
    }

    pub fn allocate_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Serialize a request to the JSON-line wire format the Python worker
    /// expects. This is the *only* place the wire format is defined.
    pub fn encode(&self, request: &BlenderRequest) -> String {
        serde_json::to_string(request).expect("BlenderRequest serializes")
    }

    /// Parse a single line of worker stdout.
    pub fn decode(line: &str) -> Result<BlenderResponse, WorkerError> {
        serde_json::from_str(line).map_err(|e| WorkerError::Reported(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_handshake_request() {
        let w = BlenderWorker::new();
        let req = BlenderRequest::Handshake {
            client_version: "0.1.0".into(),
        };
        let line = w.encode(&req);
        assert!(line.contains("\"handshake\""));
        assert!(line.contains("0.1.0"));
    }

    #[test]
    fn decode_ready_response() {
        let line = r#"{"type":"ready","blender_version":"4.1.0","worker_version":"0.1.0"}"#;
        let response = BlenderWorker::decode(line).unwrap();
        assert!(matches!(response, BlenderResponse::Ready { .. }));
    }

    #[test]
    fn lifecycle_transitions() {
        let mut w = BlenderWorker::new();
        assert_eq!(w.state(), WorkerState::Stopped);
        w.begin_start();
        assert_eq!(w.state(), WorkerState::Starting);
        w.mark_ready();
        assert_eq!(w.state(), WorkerState::Ready);
        w.mark_busy();
        w.stop();
        assert_eq!(w.state(), WorkerState::Stopped);
    }

    #[test]
    fn request_id_allocator_is_monotonic() {
        let mut w = BlenderWorker::new();
        let a = w.allocate_request_id();
        let b = w.allocate_request_id();
        let c = w.allocate_request_id();
        assert_eq!((a, b, c), (0, 1, 2));
    }
}
