import { MicOff, ShieldCheck } from "lucide-react";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";

const COLORS = [
  "bg-orange-100",
  "bg-blue-400",
  "bg-green-500",
  "bg-purple-200",
  "bg-purple-300",
  "bg-yellow-400",
  "bg-sky-300",
  "bg-orange-200",
  "bg-blue-50",
  "bg-pink-400",
  "bg-teal-300",
  "bg-rose-200",
  "bg-indigo-300",
  "bg-amber-200",
  "bg-emerald-300",
  "bg-cyan-200",
] as const;

function getRandomColor(id: number): string {
  // Use a seeded approach based on id so colors are stable across re-renders
  const seed = id * 2654435761; // Knuth multiplicative hash
  const index = Math.abs(seed) % COLORS.length;
  return COLORS[index];
}

const PARTICIPANTS = [
  { id: 1, name: "Natura", muted: false },
  { id: 2, name: "Cecile", muted: false, isVerified: true },
  { id: 3, name: "Nico", muted: false },
  { id: 4, name: "Bryan", muted: true },
  { id: 5, name: "Azzura", muted: true },
  { id: 6, name: "Ahmed", muted: true },
  { id: 7, name: "Marry", muted: false },
  { id: 8, name: "Diana", muted: false },
  { id: 9, name: "Lucas", muted: false },
  { id: 10, name: "Mike", muted: true },
  { id: 11, name: "Daniel", muted: true },
  { id: 12, name: "Shandy", muted: true },
];

export default function VideoGrid() {
  return (
    <div className="h-full w-full max-w-full p-2 sm:p-4 bg-gray-50/30 overflow-y-auto overflow-x-hidden box-border">
      <div className="grid grid-cols-2 md:grid-cols-5 gap-2 sm:gap-3 w-full max-w-full">
        {[...PARTICIPANTS]
          .sort((a, b) => a.name.localeCompare(b.name))
          .map((user) => (
            <div
              key={user.id}
              className={`relative overflow-hidden shadow-sm flex flex-col items-center justify-center transition-transform hover:scale-[1.02] border border-[#272EA766] rounded-lg w-full h-[calc((100vh-180px)/5)] md:h-[calc((100vh-180px)/2)] ${getRandomColor(user.id)}`}
            >
              {/* Participant Placeholder / Video Stream */}
              <div className="relative w-full h-full flex items-center justify-center">
                <Avatar className="h-20 w-20 md:h-24 md:w-24 border-4 border-white/20">
                  <AvatarImage
                    src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${user.name}`}
                  />
                  <AvatarFallback className="text-2xl font-bold">
                    {user.name[0]}
                  </AvatarFallback>
                </Avatar>
              </div>

              {/* Label Overlay */}
              <div
                style={{
                  backgroundColor: "#00000080",
                  borderColor: "#00000080",
                  backdropFilter: "blur(20px)",
                  borderWidth: "1px",
                  borderTopRightRadius: "10px",
                }}
                className="absolute bottom-0 left-0 flex items-center gap-2 border px-3 py-1"
              >
                {user.isVerified && (
                  <ShieldCheck className="w-3 h-3 text-blue fill-current" />
                )}
                <span className="text-white text-xs font-medium">
                  {user.name}
                </span>
              </div>

              {/* Muted Indicator Overlay */}
              {user.muted && (
                <div
                  style={{
                    backgroundColor: "#00000080",
                    borderColor: "#00000080",
                    backdropFilter: "blur(20px)",
                    borderWidth: "1px",
                    right: "20px",
                    borderTopLeftRadius: "10px",
                  }}
                  className="absolute bottom-0 py-1 px-3 rounded-t-lg"
                >
                  <MicOff className=" text-white" size={16} />
                </div>
              )}

              {/* Active Speaker Border (Optional Highlight) */}
              {user.id === 2 && (
                <div className="absolute inset-0 border-4 border-yellow-400 rounded-lg pointer-events-none" />
              )}
            </div>
          ))}
      </div>
    </div>
  );
}
