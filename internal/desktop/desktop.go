// Package desktop connects the host to an environment's desktop, served as
// VNC by the guest agent's Rust half (crates/agent/src/desktop.rs).
//
// The desktop is the image's own: a labwc session, headless, that draws into
// memory whether or not anyone is looking (hangar-desktop.service). The VNC
// server is wayvnc, attached to that session as another Wayland client, and
// started when the host first asks for the desktop.
//
// # Protocol
//
// A connection on Port starts with one JSON line, a Request. For OpVNC the
// connection is then joined to wayvnc's socket as it stands, so the host
// carries RFB without reading it; with no desktop it ends without a byte
// written. For OpResize the desktop is resized, within 640x400 to 3840x2160
// with even sides, and one JSON line, a Reply, answers.
//
// The size is set by the guest rather than by a viewer through RFB, so that
// one desktop shared by several viewers has one owner of its size.
package desktop

// Port is the vsock port the guest agent serves the desktop on.
// It is below 1024, as every port the agent listens on is (internal/agent).
const Port uint32 = 103

// Request operations.
const (
	OpVNC    = "vnc"
	OpResize = "resize"
)

// Request opens a connection.
type Request struct {
	Op     string `json:"op"`
	Width  int    `json:"width,omitempty"`
	Height int    `json:"height,omitempty"`
}

// Reply answers an OpResize.
type Reply struct {
	Err string `json:"err,omitempty"`
}
