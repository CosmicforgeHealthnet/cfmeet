/** Camera / screen-share tiles from the LiveKit room (real media, FE-004). */
import { FlatList, View, useWindowDimensions } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Track } from "livekit-client";
import { VideoTrack, isTrackReference, useTracks, type TrackReferenceOrPlaceholder } from "@livekit/react-native";
import { Text } from "@/components/ui/text";

function Tile({ trackRef, size }: { trackRef: TrackReferenceOrPlaceholder; size: number }) {
  const p = trackRef.participant;
  const name = p.name || "Participant";
  const micMuted = !p.isMicrophoneEnabled;
  const showVideo = isTrackReference(trackRef) && !trackRef.publication.isMuted;
  return (
    <View style={{ width: size, height: size * 1.2, padding: 4 }}>
      <View className="flex-1 overflow-hidden rounded-2xl bg-slate-800 items-center justify-center">
        {showVideo ? (
          <VideoTrack
            trackRef={trackRef}
            style={{ width: "100%", height: "100%" }}
            objectFit={trackRef.source === Track.Source.ScreenShare ? "contain" : "cover"}
            mirror={p.isLocal && trackRef.source === Track.Source.Camera}
          />
        ) : (
          <View className="h-16 w-16 rounded-full bg-slate-600 items-center justify-center">
            <Text className="text-2xl font-bold text-white">{name.charAt(0).toUpperCase()}</Text>
          </View>
        )}
        <View className="absolute bottom-2 left-2 right-2 flex-row items-center gap-1 rounded-lg bg-black/50 px-2 py-1">
          {micMuted && <Ionicons name="mic-off" size={12} color="#fff" />}
          {p.isSpeaking && <Ionicons name="volume-high" size={12} color="#22C55E" />}
          <Text className="text-xs text-white flex-1" numberOfLines={1}>
            {p.isLocal ? `${name} (you)` : name}
            {trackRef.source === Track.Source.ScreenShare ? " · screen" : ""}
          </Text>
        </View>
      </View>
    </View>
  );
}

export function VideoGrid() {
  const { width } = useWindowDimensions();
  const tracks = useTracks(
    [
      { source: Track.Source.Camera, withPlaceholder: true },
      { source: Track.Source.ScreenShare, withPlaceholder: false },
    ],
    { onlySubscribed: false }
  );
  const columns = tracks.length <= 1 ? 1 : 2;
  const size = (width - 16) / columns;
  return (
    <FlatList
      key={columns}
      data={tracks}
      numColumns={columns}
      contentContainerStyle={{ padding: 4 }}
      keyExtractor={(t) => `${t.participant.identity}:${t.source}`}
      renderItem={({ item }) => <Tile trackRef={item} size={size} />}
    />
  );
}
