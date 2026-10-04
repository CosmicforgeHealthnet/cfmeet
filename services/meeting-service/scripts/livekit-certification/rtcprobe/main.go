// rtcprobe — a real WebRTC media participant used to certify the meeting-service
// LiveKit integration against a self-hosted livekit-server (Phase 2 Part E).
//
// It connects with a CF Meet-issued credential, optionally publishes an Opus audio
// track, and prints one JSON line per observation:
//
//	{"event":"connected"}
//	{"event":"publish_ok","source":"MICROPHONE"} | {"event":"publish_denied","error":"…"}
//	{"event":"track_muted"} / {"event":"track_unmuted"}
//	{"event":"disconnected","reason":"…"}
//	{"event":"timeout"}
//
// Usage: rtcprobe <ws-url> <credential> <publish:true|false> <seconds>
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/livekit/protocol/livekit"
	lksdk "github.com/livekit/server-sdk-go/v2"
	"github.com/pion/webrtc/v4"
	"github.com/pion/webrtc/v4/pkg/media"
)

func emit(fields map[string]any) {
	b, _ := json.Marshal(fields)
	fmt.Println(string(b))
}

func main() {
	if len(os.Args) != 5 {
		fmt.Fprintln(os.Stderr, "usage: rtcprobe <ws-url> <credential> <publish> <seconds>")
		os.Exit(2)
	}
	url, token := os.Args[1], os.Args[2]
	publish := os.Args[3] == "true"
	seconds, _ := strconv.Atoi(os.Args[4])

	var disconnected atomic.Bool
	done := make(chan struct{})
	cb := lksdk.NewRoomCallback()
	cb.OnDisconnectedWithReason = func(reason lksdk.DisconnectionReason) {
		if disconnected.CompareAndSwap(false, true) {
			emit(map[string]any{"event": "disconnected", "reason": string(reason)})
			close(done)
		}
	}
	room, err := lksdk.ConnectToRoomWithToken(url, token, cb, lksdk.WithAutoSubscribe(true))
	if err != nil {
		emit(map[string]any{"event": "connect_failed", "error": err.Error()})
		os.Exit(1)
	}
	emit(map[string]any{"event": "connected", "identity": room.LocalParticipant.Identity()})

	var pub *lksdk.LocalTrackPublication
	if publish {
		track, err := lksdk.NewLocalTrack(webrtc.RTPCodecCapability{
			MimeType: webrtc.MimeTypeOpus, ClockRate: 48000, Channels: 2,
		})
		if err != nil {
			emit(map[string]any{"event": "publish_denied", "error": err.Error()})
		} else {
			pub, err = room.LocalParticipant.PublishTrack(track, &lksdk.TrackPublicationOptions{
				Name: "mic", Source: livekit.TrackSource_MICROPHONE,
			})
			if err != nil {
				emit(map[string]any{"event": "publish_denied", "error": err.Error()})
				pub = nil
			} else {
				emit(map[string]any{"event": "publish_ok", "source": "MICROPHONE", "trackSid": pub.SID()})
				go func() {
					silence := []byte{0xf8, 0xff, 0xfe}
					for !disconnected.Load() {
						_ = track.WriteSample(media.Sample{Data: silence, Duration: 20 * time.Millisecond}, nil)
						time.Sleep(20 * time.Millisecond)
					}
				}()
			}
		}
	}

	muted := false
	deadline := time.After(time.Duration(seconds) * time.Second)
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		select {
		case <-done:
			return
		case <-deadline:
			emit(map[string]any{"event": "timeout"})
			room.Disconnect()
			return
		case <-tick.C:
			if pub != nil && pub.IsMuted() != muted {
				muted = pub.IsMuted()
				if muted {
					emit(map[string]any{"event": "track_muted"})
				} else {
					emit(map[string]any{"event": "track_unmuted"})
				}
			}
		}
	}
}
