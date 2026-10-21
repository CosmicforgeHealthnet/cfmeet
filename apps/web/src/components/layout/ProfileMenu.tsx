"use client";

import { useState, useRef, useEffect } from "react";
import { useRouter } from "next/navigation";
import { cn } from "@/lib/utils";
import { Settings, LogOut } from "lucide-react";
import { useLogout, useProfile } from "@/hooks";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";

interface ProfileMenuProps {
  expanded: boolean;
  mobile?: boolean;
}

export default function ProfileMenu({
  expanded,
  mobile = false,
}: ProfileMenuProps) {
  const router = useRouter();
  const logout = useLogout();
  const [showProfileMenu, setShowProfileMenu] = useState(false);
  const [showLogoutDialog, setShowLogoutDialog] = useState(false);
  const profileRef = useRef<HTMLDivElement>(null);
  const { data: user } = useProfile();

  const initials = user?.username
    ? user.username.slice(0, 2).toUpperCase()
    : "U";

  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (
        profileRef.current &&
        !profileRef.current.contains(event.target as Node)
      ) {
        setShowProfileMenu(false);
      }
    }
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  const handleLogout = () => {
    logout.mutate(undefined, {
      onSuccess: () => router.push("/login"),
      onError: () => router.push("/login"),
    });
  };

  return (
    <div className="relative" ref={profileRef}>
      {showProfileMenu && (
        <div
          className={cn(
            "absolute bottom-full left-0 mb-3 w-60 bg-card rounded-xl shadow-xl border border-border p-2 z-50 animate-in fade-in slide-in-from-bottom-2",
            // Responsive positioning logic
            mobile
              ? "min-w-[200px]"
              : expanded
                ? "min-w-[200px]"
                : "min-w-[200px] left-8",
          )}
        >
          <div className="px-3 py-2 border-b border-border mb-1">
            <p className="font-medium text-sm text-foreground truncate">
              {user?.username || "User"}
            </p>
            <p className="text-xs text-muted-foreground truncate">
              {user?.email || "user@example.com"}
            </p>
          </div>

          <button
            onClick={() => router.push("/dashboard/settings")}
            className="w-full flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-primary rounded-lg transition-colors text-left"
          >
            <Settings className="w-4 h-4" />
            My Account
          </button>

          <button
            data-testid="logout-open"
            onClick={() => {
              setShowProfileMenu(false);
              setShowLogoutDialog(true);
            }}
            className="w-full flex items-center gap-2 px-3 py-2 text-sm text-red-600 hover:bg-red-50 rounded-lg transition-colors text-left"
          >
            <LogOut className="w-4 h-4" />
            Logout
          </button>
        </div>
      )}

      {/* Logout Confirmation Dialog */}
      <Dialog open={showLogoutDialog} onOpenChange={setShowLogoutDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Sign Out</DialogTitle>
            <DialogDescription>
              Are you sure you want to sign out of your account?
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowLogoutDialog(false)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              data-testid="logout-confirm"
              onClick={handleLogout}
              loading={logout.isPending}
            >
              Sign Out
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <button
        data-testid="profile-menu-toggle"
        onClick={() => setShowProfileMenu(!showProfileMenu)}
        className={cn(
          "w-full flex items-center gap-3 p-2 rounded-xl transition-all hover:bg-card hover:shadow-sm border border-transparent hover:border-border group",
          showProfileMenu && "bg-card shadow-sm border-border",
          !expanded && "rounded-full w-10 h-10 p-0 justify-center",
        )}
      >
        <div className="relative">
          <Avatar className="w-9 h-9 border border-border shrink-0">
            <AvatarFallback className="bg-primary/10 text-primary text-xs font-semibold">
              {initials}
            </AvatarFallback>
          </Avatar>
          <span className="absolute bottom-0 right-0 w-2.5 h-2.5 bg-green-500 border-2 border-background rounded-full z-10" />
        </div>

        {expanded && (
          <div className="flex-1 text-left overflow-hidden">
            <p className="text-sm font-medium text-foreground truncate group-hover:text-primary transition-colors">
              {user?.username || "Profile"}
            </p>
            <p className="text-xs text-muted-foreground truncate">View Profile</p>
          </div>
        )}
      </button>
    </div>
  );
}
