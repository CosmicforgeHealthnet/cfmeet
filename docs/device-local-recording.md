# Device recording for meetings and calls

Recording in the CF Meet web app uses the device's browser recorder. It combines
the subscribed audio, the user's published microphone, cameras and shared screens
into a video file. Muted or stopped tracks are excluded. No recording bytes are
uploaded, attached to an account, or sent by email. The existing server recording
feature and its activation campaign stay off. Transcripts and AI remain off.

In a meeting, the host's recording policy and start permission govern the button.
In an admitted app call, participants can request recording. Everyone currently in
the media session must explicitly allow the request within 30 seconds. Reliable
room messages carry the request, decision and visible recording notice. Unsupported
or older clients cannot silently count as consenting. A decline, consent withdrawal,
participant join/leave or reconnect stops the recording. Consent must be requested
again before restarting. Local recording is not a server recording authorization.

Choose **Record meeting/call to this device**, then **Stop & save**. The browser
downloads a WebM or MP4 file, according to its supported format. **Save recording
again** remains available in the session. Leaving ends capture and attempts the
download; stop and save before leaving for the clearest download flow. Closing or
crashing the browser before the download finishes can lose an unfinished recording.
Keep the recording page active; browsers can throttle background video drawing.

The current memory-backed browser implementation automatically stops and saves at
256 MiB. Start another part after saving. It does not claim disk streaming, native
iOS/Android recording, or support in browsers lacking MediaRecorder/canvas capture.
Native mobile clients do not acquire a new recorder through a web deployment.

Calling's APP_MEDIA_V2 defaults allow data publishing for newly admitted app
participants so consent and notices can be exchanged. Existing persisted V1
permissions are preserved; start a new call for this capability. Join-only grants,
terminal sessions and public bridge permissions remain restricted.

Validation: browser test records synthetic video and audible audio, exercises track
removal/re-addition, plays the resulting file, and checks source tracks remain live.
Consent tests cover missing, foreign, duplicate and stale approvals and withdrawal.
Production requires the Calling backend update as well as the frontend deployment.
