import {
  Volume2,
  VolumeX,
  Upload,
  Mic,
  MicOff,
  Video,
  VideoOff,
  Maximize,
  Phone,
  ChevronUp,
  Check,
} from "lucide-react";
import { Button } from "../ui/button";
import {
  useLocalParticipant,
  useRoomContext,
  useMediaDeviceSelect,
} from "@livekit/components-react";
import { useMemo, useState } from "react";
import { MicVisualizer } from "../ui/mic-visualizer";
import { useMediaControl } from "@/hooks";

interface FooterControlsProps {
  onToggleSidebar: () => void;
  isSidebarOpen: boolean;
  meetingId: string;
  participantId: string;
}

export default function FooterControls({
  onToggleSidebar,
  isSidebarOpen,
  meetingId,
  participantId,
}: FooterControlsProps) {
  const {
    localParticipant,
    isMicrophoneEnabled,
    isCameraEnabled,
    isScreenShareEnabled,
    microphoneTrack,
  } = useLocalParticipant();
  const room = useRoomContext();
  const [isSpeakerOn, setIsSpeakerOn] = useState(true);

  const { startScreenShare, stopScreenShare, updateAudio, updateVideo } =
    useMediaControl();

  const {
    devices: audioOutputDevices,
    activeDeviceId: activeAudioOutputDeviceId,
    setActiveMediaDevice: setActiveAudioOutputDevice,
  } = useMediaDeviceSelect({ kind: "audiooutput" });

  const [isDeviceMenuOpen, setIsDeviceMenuOpen] = useState(false);

  const toggleSpeaker = () => {
    const newState = !isSpeakerOn;
    setIsSpeakerOn(newState);
    // Mute/unmute all audio elements in the room
    const audioElements = document.querySelectorAll("audio");
    audioElements.forEach((audio) => {
      audio.muted = !newState;
    });
  };

  const mediaStreamTrack = microphoneTrack?.track?.mediaStreamTrack;
  const activeStream = useMemo(() => {
    if (mediaStreamTrack) {
      return new MediaStream([mediaStreamTrack]);
    }
    return null;
  }, [mediaStreamTrack]);

  const toggleMic = async () => {
    const newState = !isMicrophoneEnabled;
    await localParticipant.setMicrophoneEnabled(newState, {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
    });
    updateAudio.mutate({ participantId, isMuted: !newState });
  };

  const toggleCamera = async () => {
    const newState = !isCameraEnabled;
    await localParticipant.setCameraEnabled(newState);
    updateVideo.mutate({ participantId, isVideoOn: newState });
  };

  const toggleScreenShare = async () => {
    const newState = !isScreenShareEnabled;
    await localParticipant.setScreenShareEnabled(newState);
    if (newState) {
      startScreenShare.mutate(meetingId);
    } else {
      stopScreenShare.mutate(meetingId);
    }
  };

  const handleDisconnect = () => {
    room.disconnect();
  };

  return (
    <footer className="p-2 sm:p-6 flex items-center justify-center gap-2 sm:gap-4 bg-card border-t border-border z-10 relative">
      <div className="flex flex-wrap items-center justify-between w-full max-w-5xl rounded-lg gap-2 sm:gap-4 px-1 sm:px-6">
        {/* Audio Controls Group */}
        <div className="relative flex items-center gap-1 bg-card/50 rounded-lg p-1">
          <Button
            onClick={toggleSpeaker}
            className={`p-2 sm:p-3 rounded-md transition-colors ${
              !isSpeakerOn
                ? "bg-red-50 text-red-500 hover:bg-red-100"
                : "bg-sidebar text-primary hover:bg-muted"
            }`}
          >
            {isSpeakerOn ? (
              <Volume2 className="text-primary" size={24} />
            ) : (
              <VolumeX className="text-red-500" size={24} />
            )}
          </Button>

          <Button
            onClick={() => setIsDeviceMenuOpen(!isDeviceMenuOpen)}
            className="p-1 h-full rounded-md bg-sidebar text-primary hover:bg-muted"
          >
            <ChevronUp size={16} />
          </Button>

          {isDeviceMenuOpen && (
            <div className="absolute bottom-full left-0 mb-2 w-64 bg-card border border-border rounded-lg shadow-xl overflow-hidden z-50">
              <div className="p-2 border-b border-border text-xs font-semibold text-muted-foreground uppercase tracking-wider">
                Select Audio Output
              </div>
              <div className="max-h-60 overflow-y-auto">
                {audioOutputDevices.map((device) => (
                  <button
                    key={device.deviceId}
                    onClick={() => {
                      setActiveAudioOutputDevice(device.deviceId);
                      setIsDeviceMenuOpen(false);
                    }}
                    className={`w-full text-left px-3 py-2 text-sm flex items-center justify-between hover:bg-muted ${
                      activeAudioOutputDeviceId === device.deviceId
                        ? "text-sky-600 bg-sky-50"
                        : "text-foreground"
                    }`}
                  >
                    <span className="truncate">{device.label}</span>
                    {activeAudioOutputDeviceId === device.deviceId && (
                      <Check size={16} />
                    )}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
        <div className="flex items-center gap-2 sm:gap-4">
          <Button
            onClick={toggleScreenShare}
            className={`p-2 sm:p-3 rounded-lg transition-colors ${
              isScreenShareEnabled
                ? "bg-green-100 text-green-600 hover:bg-green-200"
                : "bg-sidebar text-primary hover:bg-muted"
            }`}
          >
            <Upload
              className={
                isScreenShareEnabled ? "text-green-600" : "text-primary"
              }
              size={24}
            />
          </Button>

          <div className="relative">
            <Button
              onClick={toggleMic}
              className={`p-2 sm:p-3 rounded-lg transition-colors relative z-10 ${
                !isMicrophoneEnabled
                  ? "bg-red-50 text-red-500 hover:bg-red-100"
                  : "bg-sidebar text-primary hover:bg-muted"
              }`}
            >
              {isMicrophoneEnabled ? (
                <Mic className="text-primary" size={24} />
              ) : (
                <MicOff className="text-red-500" size={24} />
              )}
            </Button>

            {/* Visualizer Overlay */}
            {/* Visualizer Overlay */}
            {isMicrophoneEnabled && (
              <div className="absolute inset-0 flex items-end justify-center pb-2 pointer-events-none z-20">
                <MicVisualizer
                  stream={activeStream}
                  isOn={true}
                  barColor="bg-green-400"
                  count={4}
                  className="h-2"
                />
              </div>
            )}
          </div>

          {/* ... */}

          <Button
            onClick={handleDisconnect}
            style={{
              width: "56px",
              height: "56px",
              backgroundColor: "#FF3639",
              borderRadius: "28px",
            }}
            className="text-white shadow-xl hover:bg-red-600 transition-colors flex items-center justify-center p-0"
            variant="destructive"
          >
            <Phone className="fill-current rotate-[135deg]" size={28} />
          </Button>

          <Button
            onClick={toggleCamera}
            className={`p-2 sm:p-3 rounded-lg transition-colors ${
              !isCameraEnabled
                ? "bg-red-50 text-red-500 hover:bg-red-100"
                : "bg-sidebar text-primary hover:bg-muted"
            }`}
          >
            {isCameraEnabled ? (
              <Video className="text-primary" size={24} />
            ) : (
              <VideoOff className="text-red-500" size={24} />
            )}
          </Button>

        </div>
        <Button
          onClick={onToggleSidebar}
          className={`p-2 sm:p-3 rounded-lg transition-colors hover:text-white ${
            isSidebarOpen
              ? "bg-sky-100 text-sky-600"
              : "bg-muted text-muted-foreground"
          }`}
        >
          <Maximize size={24} />
        </Button>
      </div>
    </footer>
  );
}
