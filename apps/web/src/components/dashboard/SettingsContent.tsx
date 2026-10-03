"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { userService } from "@/services/user.service";
import { UserProfile } from "@/types/user";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Label } from "@/components/ui/label";
import { storageStore } from "@/store";
import ApiKeysContent from "./ApiKeysContent";
import WebhooksContent from "./WebhooksContent";
import { User, Key, Webhook } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";

export default function SettingsContent() {
  const [activeTab, setActiveTab] = useState<"profile" | "api-keys" | "webhooks">("profile");

  return (
    <div className="max-w-4xl mx-auto p-6 space-y-8">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">Settings</h1>
        <p className="text-muted-foreground text-gray-500">
          Manage your account settings and preferences.
        </p>
      </div>

      <div className="flex border-b border-gray-100 overflow-x-auto">
        <button
          onClick={() => setActiveTab("profile")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "profile"
              ? "border-[#029CD4] text-[#029CD4]"
              : "border-transparent text-gray-400 hover:text-gray-600",
          )}
        >
          <User className="w-4 h-4" />
          Profile
        </button>
        <button
          onClick={() => setActiveTab("api-keys")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "api-keys"
              ? "border-[#029CD4] text-[#029CD4]"
              : "border-transparent text-gray-400 hover:text-gray-600",
          )}
        >
          <Key className="w-4 h-4" />
          API Keys
        </button>
        <button
          onClick={() => setActiveTab("webhooks")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "webhooks"
              ? "border-[#029CD4] text-[#029CD4]"
              : "border-transparent text-gray-400 hover:text-gray-600",
          )}
        >
          <Webhook className="w-4 h-4" />
          Webhooks
        </button>
      </div>

      {activeTab === "profile" && <ProfileSettings />}
      {activeTab === "api-keys" && <ApiKeysContent />}
      {activeTab === "webhooks" && <WebhooksContent />}
    </div>
  );
}

function ProfileSettings() {
  const router = useRouter();
  const [profile, setProfile] = useState<UserProfile | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [success, setSuccess] = useState("");
  const [newUsername, setNewUsername] = useState("");
  const [updating, setUpdating] = useState(false);
  const [deactivating, setDeactivating] = useState(false);

  useEffect(() => {
    fetchProfile();
  }, []);

  const fetchProfile = async () => {
    try {
      setLoading(true);
      const data = await userService.getProfile();
      setProfile(data);
      setNewUsername(data.username);
    } catch {
      setError("Failed to load profile");
    } finally {
      setLoading(false);
    }
  };

  const handleUpdate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!profile) return;

    // Reset messages
    setError("");
    setSuccess("");

    if (!newUsername.trim()) {
      setError("Username cannot be empty");
      return;
    }

    try {
      setUpdating(true);
      const updated = await userService.updateProfile({
        username: newUsername,
      });
      setProfile(updated);
      setSuccess("Profile updated successfully");

      // Update local storage if needed
      const currentUser = storageStore.getUser();
      if (currentUser) {
        storageStore.setUser({
          ...currentUser,
          username: updated.username,
        });
      }
    } catch (err) {
      if (
        err &&
        typeof err === "object" &&
        "response" in err &&
        err.response &&
        typeof err.response === "object" &&
        "status" in err.response &&
        err.response.status === 409
      ) {
        setError("Username is already taken");
      } else {
        setError("Failed to update profile");
      }
    } finally {
      setUpdating(false);
    }
  };

  const [showDeactivateDialog, setShowDeactivateDialog] = useState(false);

  const handleDeactivate = async () => {
    try {
      setDeactivating(true);
      await userService.deactivateAccount();
      // Clear data and redirect
      storageStore.clearUser();
      router.push("/login");
    } catch {
      setError("Failed to deactivate account");
      setDeactivating(false);
      setShowDeactivateDialog(false);
    }
  };

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Spinner />
      </div>
    );
  }

  return (
    <div className="space-y-6 animate-in fade-in slide-in-from-bottom-4 duration-500">
      {/* Profile Section */}
      <div className="bg-white p-6 rounded-2xl shadow-sm border border-gray-100 space-y-6">
        <div>
          <h2 className="text-lg font-semibold mb-1">Profile Information</h2>
          <p className="text-sm text-gray-500">Update your account details.</p>
        </div>

        {error && (
          <div className="bg-red-50 text-red-600 px-4 py-3 rounded-lg text-sm border border-red-100">
            {error}
          </div>
        )}

        {success && (
          <div className="bg-green-50 text-green-600 px-4 py-3 rounded-lg text-sm border border-green-100">
            {success}
          </div>
        )}

        <form onSubmit={handleUpdate} className="space-y-4 max-w-xl">
          <div className="space-y-2">
            <Label htmlFor="email">Email Address</Label>
            <Input
              id="email"
              value={profile?.email || ""}
              disabled
              className="bg-gray-50 text-gray-500 border-gray-200"
            />
            <p className="text-xs text-gray-400">Email cannot be changed.</p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="username">Username</Label>
            <Input
              id="username"
              value={newUsername}
              onChange={(e) => setNewUsername(e.target.value)}
              className="border-gray-200 focus:border-primary/50"
            />
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label>Role</Label>
              <div className="px-3 py-2 bg-gray-50 rounded-md text-sm text-gray-700 capitalize border border-gray-100">
                {profile?.role}
              </div>
            </div>

            <div className="space-y-2">
              <Label>Status</Label>
              <div className="px-3 py-2 bg-gray-50 rounded-md text-sm text-gray-700 capitalize border border-gray-100">
                {profile?.status}
              </div>
            </div>
          </div>

          <div className="pt-2">
            <Button
              type="submit"
              loading={updating}
              disabled={updating || newUsername === profile?.username}
            >
              Save Changes
            </Button>
          </div>
        </form>
      </div>

      {/* Danger Zone */}
      <div className="bg-red-50 p-6 rounded-2xl border border-red-100 space-y-6">
        <div>
          <h2 className="text-lg font-semibold text-red-700 mb-1">
            Danger Zone
          </h2>
          <p className="text-sm text-red-600/80">
            Irreversible actions for your account.
          </p>
        </div>

        <div className="flex flex-col sm:flex-row sm:items-center justify-between p-4 bg-white rounded-xl border border-red-100 gap-4 sm:gap-0">
          <div className="space-y-1">
            <p className="font-medium text-gray-900">Deactivate Account</p>
            <p className="text-sm text-gray-500">
              This will immediately invalidate all your sessions and prevent you
              from logging in.
            </p>
          </div>
          <Dialog
            open={showDeactivateDialog}
            onOpenChange={setShowDeactivateDialog}
          >
            <DialogTrigger asChild>
              <Button variant="destructive" className="w-full sm:w-auto">
                Deactivate
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Deactivate Account</DialogTitle>
                <DialogDescription>
                  Are you absolutely sure you want to dismiss your account? This
                  action cannot be undone and will immediately prevent you from
                  accessing your account.
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <Button
                  variant="outline"
                  onClick={() => setShowDeactivateDialog(false)}
                  disabled={deactivating}
                >
                  Cancel
                </Button>
                <Button
                  variant="destructive"
                  onClick={handleDeactivate}
                  loading={deactivating}
                >
                  Deactivate Account
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </div>
      </div>
    </div>
  );
}
