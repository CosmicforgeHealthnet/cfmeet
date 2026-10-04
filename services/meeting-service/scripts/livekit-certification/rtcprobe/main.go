// rtcprobe — a real WebRTC media participant used to certify the meeting-service
// LiveKit integration against a self-hosted livekit-server (Phase 2 Parts E and F).
//
// It connects with a CF Meet-issued credential, optionally publishes tracks, and
// prints one JSON line per observation:
//
//	{"event":"connected"}
//	{"event":"publish_ok","source":"MICROPHONE","trackSid":"…"} | {"event":"publish_denied","source":"…","error":"…"}
//	{"event":"track_muted","source":"…"} / {"event":"track_unmuted","source":"…"}
//	{"event":"track_unpublished","source":"…","trackSid":"…"}   (server-side unpublish)
//	{"event":"disconnected","reason":"…"}
//	{"event":"timeout"}
//
// Usage: rtcprobe <ws-url> <credential> <publish> <seconds>
//
//	publish: false | true (= mic) | comma list of mic,camera,screen
//
// While running, a stdin line "publish <mic|camera|screen>" attempts another publish
// (used to prove that restored permissions work on a live connection).
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/livekit/protocol/livekit"
	lksdk "github.com/livekit/server-sdk-go/v2"
	"github.com/pion/webrtc/v4"
	"github.com/pion/webrtc/v4/pkg/media"
)

var out sync.Mutex

func emit(fields map[string]any) {
	b, _ := json.Marshal(fields)
	out.Lock()
	fmt.Println(string(b))
	out.Unlock()
}

type published struct {
	source string
	pub    *lksdk.LocalTrackPublication
	muted  bool
}

func sourceOf(name string) (livekit.TrackSource, string, bool) {
	switch name {
	case "mic", "true":
		return livekit.TrackSource_MICROPHONE, "MICROPHONE", true
	case "camera":
		return livekit.TrackSource_CAMERA, "CAMERA", true
	case "screen":
		return livekit.TrackSource_SCREEN_SHARE, "SCREEN_SHARE", true
	}
	return 0, "", false
}

func main() {
	if len(os.Args) != 5 {
		fmt.Fprintln(os.Stderr, "usage: rtcprobe <ws-url> <credential> <publish> <seconds>")
		os.Exit(2)
	}
	url, token := os.Args[1], os.Args[2]
	seconds, _ := strconv.Atoi(os.Args[4])

	var disconnected atomic.Bool
	done := make(chan struct{})
	var mu sync.Mutex
	pubs := map[string]*published{}

	cb := lksdk.NewRoomCallback()
	cb.OnDisconnectedWithReason = func(reason lksdk.DisconnectionReason) {
		if disconnected.CompareAndSwap(false, true) {
			emit(map[string]any{"event": "disconnected", "reason": string(reason)})
			close(done)
		}
	}
	cb.ParticipantCallback.OnLocalTrackUnpublished = func(p *lksdk.LocalTrackPublication, _ *lksdk.LocalParticipant) {
		mu.Lock()
		defer mu.Unlock()
		for k, v := range pubs {
			if v.pub.SID() == p.SID() {
				emit(map[string]any{"event": "track_unpublished", "source": v.source, "trackSid": p.SID()})
				delete(pubs, k)
			}
		}
	}
	room, err := lksdk.ConnectToRoomWithToken(url, token, cb, lksdk.WithAutoSubscribe(true))
	if err != nil {
		emit(map[string]any{"event": "connect_failed", "error": err.Error()})
		os.Exit(1)
	}
	emit(map[string]any{"event": "connected", "identity": room.LocalParticipant.Identity()})

	publish := func(name string) {
		src, label, ok := sourceOf(name)
		if !ok {
			return
		}
		codec := webrtc.RTPCodecCapability{MimeType: webrtc.MimeTypeVP8, ClockRate: 90000}
		frame, every := []byte{0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a}, 33*time.Millisecond
		if src == livekit.TrackSource_MICROPHONE {
			codec = webrtc.RTPCodecCapability{MimeType: webrtc.MimeTypeOpus, ClockRate: 48000, Channels: 2}
			frame, every = []byte{0xf8, 0xff, 0xfe}, 20*time.Millisecond
		}
		track, err := lksdk.NewLocalTrack(codec)
		if err != nil {
			emit(map[string]any{"event": "publish_denied", "source": label, "error": err.Error()})
			return
		}
		pub, err := room.LocalParticipant.PublishTrack(track, &lksdk.TrackPublicationOptions{
			Name: strings.ToLower(label), Source: src,
		})
		if err != nil {
			emit(map[string]any{"event": "publish_denied", "source": label, "error": err.Error()})
			return
		}
		mu.Lock()
		pubs[pub.SID()] = &published{source: label, pub: pub}
		mu.Unlock()
		emit(map[string]any{"event": "publish_ok", "source": label, "trackSid": pub.SID()})
		go func() {
			for !disconnected.Load() {
				_ = track.WriteSample(media.Sample{Data: frame, Duration: every}, nil)
				time.Sleep(every)
			}
		}()
	}

	switch os.Args[3] {
	case "false", "":
	default:
		for _, name := range strings.Split(os.Args[3], ",") {
			publish(strings.TrimSpace(name))
		}
	}

	go func() {
		scanner := bufio.NewScanner(os.Stdin)
		for scanner.Scan() {
			parts := strings.Fields(scanner.Text())
			if len(parts) == 2 && parts[0] == "publish" {
				publish(parts[1])
			}
		}
	}()

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
			mu.Lock()
			for _, p := range pubs {
				if p.pub.IsMuted() != p.muted {
					p.muted = p.pub.IsMuted()
					ev := "track_unmuted"
					if p.muted {
						ev = "track_muted"
					}
					emit(map[string]any{"event": ev, "source": p.source})
				}
			}
			mu.Unlock()
		}
	}
}
