"use client";

import { useState } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  BookOpen,
  Code2,
  Key,
  Globe,
  Server,
  ShieldCheck,
  Zap,
  Webhook,
  Copy,
  Check,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { useRouter } from "next/navigation";
import { cn } from "@/lib/utils";

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <button
      onClick={handleCopy}
      className="absolute top-2 right-2 p-1.5 rounded bg-gray-700 hover:bg-gray-600 transition-colors"
    >
      {copied ? (
        <Check className="w-3.5 h-3.5 text-green-400" />
      ) : (
        <Copy className="w-3.5 h-3.5 text-gray-400" />
      )}
    </button>
  );
}

function CodeBlock({ code, language = "bash" }: { code: string; language?: string }) {
  return (
    <div className="relative bg-[#1E1E1E] rounded-lg overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 bg-[#2D2D2D] border-b border-gray-700">
        <span className="text-xs text-gray-400 font-mono">{language}</span>
      </div>
      <pre className="p-4 overflow-x-auto">
        <code className="text-sm font-mono text-gray-300 whitespace-pre">{code}</code>
      </pre>
      <CopyButton text={code} />
    </div>
  );
}

function CollapsibleSection({
  title,
  children,
  defaultOpen = false,
}: {
  title: string;
  children: React.ReactNode;
  defaultOpen?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(defaultOpen);

  return (
    <div className="border border-gray-200 rounded-xl overflow-hidden">
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="w-full flex items-center justify-between p-4 bg-gray-50 hover:bg-gray-100 transition-colors text-left"
      >
        <span className="font-medium text-[#343434]">{title}</span>
        {isOpen ? (
          <ChevronDown className="w-5 h-5 text-gray-500" />
        ) : (
          <ChevronRight className="w-5 h-5 text-gray-500" />
        )}
      </button>
      {isOpen && <div className="p-4 bg-white space-y-4">{children}</div>}
    </div>
  );
}

export default function DevelopersContent() {
  const router = useRouter();
  const [activeTab, setActiveTab] = useState<"overview" | "api" | "webhooks">("overview");

  return (
    <div className="h-full flex flex-col font-inter bg-white/50">
      <div className="flex items-center justify-between p-6 md:p-8 border-b border-gray-100 bg-white">
        <div>
          <h1 className="text-2xl font-bold text-[#343434] flex items-center gap-2">
            <Code2 className="w-8 h-8 text-[#029CD4]" />
            Developer Guide
          </h1>
          <p className="text-[#00000080] mt-1">
            Build powerful real-time video applications with CosmicForge RTC
          </p>
        </div>
        <Button
          onClick={() => router.push("/dashboard/settings")}
          variant="outline"
          className="hidden sm:flex"
        >
          <Key className="w-4 h-4 mr-2" />
          Get API Keys
        </Button>
      </div>

      {/* Tabs */}
      <div className="flex border-b border-gray-100 px-6 md:px-8 bg-white overflow-x-auto">
        <button
          onClick={() => setActiveTab("overview")}
          className={cn(
            "flex items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "overview"
              ? "border-[#029CD4] text-[#029CD4]"
              : "border-transparent text-gray-400 hover:text-gray-600"
          )}
        >
          <Globe className="w-4 h-4" />
          Overview
        </button>
        <button
          onClick={() => setActiveTab("api")}
          className={cn(
            "flex items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "api"
              ? "border-[#029CD4] text-[#029CD4]"
              : "border-transparent text-gray-400 hover:text-gray-600"
          )}
        >
          <Server className="w-4 h-4" />
          API Reference
        </button>
        <button
          onClick={() => setActiveTab("webhooks")}
          className={cn(
            "flex items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "webhooks"
              ? "border-[#029CD4] text-[#029CD4]"
              : "border-transparent text-gray-400 hover:text-gray-600"
          )}
        >
          <Webhook className="w-4 h-4" />
          Webhooks
        </button>
      </div>

      <ScrollArea className="flex-1">
        <div className="p-6 md:p-8 max-w-4xl mx-auto space-y-12">
          {activeTab === "overview" && <OverviewTab router={router} />}
          {activeTab === "api" && <ApiReferenceTab router={router} />}
          {activeTab === "webhooks" && <WebhooksTab router={router} />}
        </div>
      </ScrollArea>
    </div>
  );
}

function OverviewTab({ router }: { router: ReturnType<typeof useRouter> }) {
  return (
    <>
      {/* Introduction */}
      <section className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-lg bg-[#E6F5FA] flex items-center justify-center text-[#029CD4]">
            <Globe className="w-5 h-5" />
          </div>
          <h2 className="text-xl font-bold text-[#343434]">
            What is CosmicForge RTC?
          </h2>
        </div>
        <div className="pl-[52px] space-y-4 text-gray-600 leading-relaxed">
          <p>
            CosmicForge RTC is a developer-first platform designed to
            simplify the integration of real-time audio and video
            communications into your applications. Whether you&apos;re building a
            telehealth platform, an educational tool, or a social video app,
            specific infrastructure often becomes a bottleneck.
          </p>
          <p>
            We provide a robust, scalable Backend-as-a-Service (BaaS)
            solution that handles the heavy lifting—signaling, media
            transport, and session management—so you can focus on building
            your frontend experience.
          </p>
        </div>
      </section>

      {/* Core Features */}
      <section className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-lg bg-[#E6F5FA] flex items-center justify-center text-[#029CD4]">
            <Zap className="w-5 h-5" />
          </div>
          <h2 className="text-xl font-bold text-[#343434]">
            Core Capabilities
          </h2>
        </div>
        <div className="pl-[52px] grid sm:grid-cols-2 gap-4">
          <div className="p-4 rounded-xl border border-gray-100 bg-white shadow-sm hover:shadow-md transition-shadow">
            <h3 className="font-semibold text-[#343434] mb-2">
              Instant Meetings
            </h3>
            <p className="text-sm text-gray-500">
              Create and join meetings instantly with low-latency signaling
              and high-quality media streams.
            </p>
          </div>
          <div className="p-4 rounded-xl border border-gray-100 bg-white shadow-sm hover:shadow-md transition-shadow">
            <h3 className="font-semibold text-[#343434] mb-2">
              Secure Rooms
            </h3>
            <p className="text-sm text-gray-500">
              Protect your sessions with private meeting modes and
              role-based access control.
            </p>
          </div>
          <div className="p-4 rounded-xl border border-gray-100 bg-white shadow-sm hover:shadow-md transition-shadow">
            <h3 className="font-semibold text-[#343434] mb-2">
              Real-time Webhooks
            </h3>
            <p className="text-sm text-gray-500">
              Receive instant notifications for meeting starts, ends, and
              participant join/leave events.
            </p>
          </div>
          <div className="p-4 rounded-xl border border-gray-100 bg-white shadow-sm hover:shadow-md transition-shadow">
            <h3 className="font-semibold text-[#343434] mb-2">
              Developer Friendly
            </h3>
            <p className="text-sm text-gray-500">
              Simple REST API for management and WebSocket interfaces for
              real-time interaction.
            </p>
          </div>
        </div>
      </section>

      {/* Quick Start Guide */}
      <section className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-lg bg-[#E6F5FA] flex items-center justify-center text-[#029CD4]">
            <BookOpen className="w-5 h-5" />
          </div>
          <h2 className="text-xl font-bold text-[#343434]">
            Getting Started
          </h2>
        </div>

        <div className="pl-[52px] space-y-6">
          {/* Step 1 */}
          <div className="relative border-l-2 border-gray-200 pl-6 pb-2">
            <div className="absolute -left-[9px] top-0 w-4 h-4 rounded-full bg-white border-2 border-[#029CD4]" />
            <h3 className="font-semibold text-[#343434] mb-1">
              1. Generate an API Key
            </h3>
            <p className="text-gray-600 mb-3">
              Navigate to the{" "}
              <span
                className="font-medium text-[#029CD4] cursor-pointer hover:underline"
                onClick={() => router.push("/dashboard/settings")}
              >
                Settings Page
              </span>{" "}
              and create a new API Key. Each key has a limit of 100 requests
              and expires after 20 days.
            </p>
            <div className="bg-[#1E1E1E] rounded-md p-3 font-mono text-xs text-green-400 overflow-x-auto">
              Api-Key: cf_live_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
            </div>
          </div>

          {/* Step 2 */}
          <div className="relative border-l-2 border-gray-200 pl-6 pb-2">
            <div className="absolute -left-[9px] top-0 w-4 h-4 rounded-full bg-white border-2 border-[#029CD4]" />
            <h3 className="font-semibold text-[#343434] mb-1">
              2. Create a Meeting
            </h3>
            <p className="text-gray-600 mb-3">
              Make a POST request to create a new meeting room.
            </p>
            <CodeBlock
              language="bash"
              code={`curl -X POST https://api.cosmicforge.io/api/v1/api/meetings \\
  -H "Content-Type: application/json" \\
  -H "Api-Key: cf_live_your_api_key" \\
  -d '{
    "title": "Team Standup",
    "description": "Daily standup meeting",
    "is_private": false
  }'`}
            />
          </div>

          {/* Step 3 */}
          <div className="relative border-l-2 border-gray-200 pl-6 pb-2">
            <div className="absolute -left-[9px] top-0 w-4 h-4 rounded-full bg-white border-2 border-[#029CD4]" />
            <h3 className="font-semibold text-[#343434] mb-1">
              3. Join the Meeting
            </h3>
            <p className="text-gray-600 mb-3">
              Use the meeting identifier to join participants to the room.
            </p>
            <CodeBlock
              language="bash"
              code={`curl -X POST https://api.cosmicforge.io/api/v1/api/meetings/{meeting_id}/join \\
  -H "Content-Type: application/json" \\
  -H "Api-Key: cf_live_your_api_key" \\
  -d '{
    "participant_name": "John Doe"
  }'`}
            />
          </div>

          {/* Step 4 */}
          <div className="relative border-l-2 border-transparent pl-6 pb-2">
            <div className="absolute -left-[9px] top-0 w-4 h-4 rounded-full bg-white border-2 border-[#029CD4]" />
            <h3 className="font-semibold text-[#343434] mb-1">
              4. Configure Webhooks (Optional)
            </h3>
            <p className="text-gray-600">
              Set up webhooks to receive real-time notifications for meeting
              events. Go to{" "}
              <span
                className="font-medium text-[#029CD4] cursor-pointer hover:underline"
                onClick={() => router.push("/dashboard/settings")}
              >
                Settings → Webhooks
              </span>{" "}
              to configure your endpoints.
            </p>
          </div>
        </div>
      </section>

      {/* Integration Model */}
      <section className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-lg bg-[#E6F5FA] flex items-center justify-center text-[#029CD4]">
            <ShieldCheck className="w-5 h-5" />
          </div>
          <h2 className="text-xl font-bold text-[#343434]">
            Security & Authentication
          </h2>
        </div>
        <div className="pl-[52px] space-y-4">
          <div className="bg-[#FAFAFB] border border-gray-200 rounded-xl p-6">
            <h4 className="font-medium text-[#343434] mb-3 flex items-center gap-2">
              <ShieldCheck className="w-4 h-4 text-green-500" />
              API Key Authentication
            </h4>
            <p className="text-sm text-gray-600 leading-relaxed mb-4">
              All third-party API requests must include your API key in the
              <code className="mx-1 px-1.5 py-0.5 bg-gray-100 rounded text-xs">Api-Key</code>
              header. Keep your API keys secure and never expose them in
              client-side code.
            </p>
            <div className="space-y-2 text-sm text-gray-600">
              <p><strong>Rate Limit:</strong> 100 requests per API key</p>
              <p><strong>Key Expiration:</strong> 20 days from creation</p>
              <p><strong>Key Format:</strong> cf_live_[64 characters]</p>
            </div>
          </div>
        </div>
      </section>
    </>
  );
}

function ApiReferenceTab({ router }: { router: ReturnType<typeof useRouter> }) {
  return (
    <>
      <section className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-lg bg-[#E6F5FA] flex items-center justify-center text-[#029CD4]">
            <Server className="w-5 h-5" />
          </div>
          <h2 className="text-xl font-bold text-[#343434]">
            API Reference
          </h2>
        </div>
        <p className="pl-[52px] text-gray-600">
          All API endpoints use the base URL:{" "}
          <code className="px-2 py-1 bg-gray-100 rounded text-sm">
            https://api.cosmicforge.io/api/v1
          </code>
        </p>
      </section>

      {/* Authentication */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Authentication</h3>
        <div className="bg-amber-50 border border-amber-200 rounded-xl p-4">
          <p className="text-sm text-amber-800">
            <strong>Important:</strong> Include your API key in the{" "}
            <code className="px-1.5 py-0.5 bg-amber-100 rounded">Api-Key</code>{" "}
            header for all requests.
          </p>
        </div>
        <CodeBlock
          language="http"
          code={`Api-Key: cf_live_your_api_key_here`}
        />
      </section>

      {/* Meetings Endpoints */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Meetings</h3>

        <CollapsibleSection title="POST /api/meetings - Create Meeting" defaultOpen>
          <p className="text-sm text-gray-600 mb-4">
            Creates a new meeting room that participants can join.
          </p>

          <h4 className="font-medium text-sm text-gray-700 mb-2">Request Body</h4>
          <div className="overflow-x-auto">
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-50">
                  <th className="text-left p-2 border border-gray-200 font-medium">Field</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Type</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Required</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td className="p-2 border border-gray-200"><code>title</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">Yes</td>
                  <td className="p-2 border border-gray-200">Meeting title</td>
                </tr>
                <tr>
                  <td className="p-2 border border-gray-200"><code>description</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">No</td>
                  <td className="p-2 border border-gray-200">Meeting description</td>
                </tr>
                <tr>
                  <td className="p-2 border border-gray-200"><code>is_private</code></td>
                  <td className="p-2 border border-gray-200">boolean</td>
                  <td className="p-2 border border-gray-200">No</td>
                  <td className="p-2 border border-gray-200">If true, requires host approval to join</td>
                </tr>
                <tr>
                  <td className="p-2 border border-gray-200"><code>start_time</code></td>
                  <td className="p-2 border border-gray-200">string (ISO 8601)</td>
                  <td className="p-2 border border-gray-200">No</td>
                  <td className="p-2 border border-gray-200">Scheduled start time</td>
                </tr>
                <tr>
                  <td className="p-2 border border-gray-200"><code>end_time</code></td>
                  <td className="p-2 border border-gray-200">string (ISO 8601)</td>
                  <td className="p-2 border border-gray-200">No</td>
                  <td className="p-2 border border-gray-200">Scheduled end time</td>
                </tr>
              </tbody>
            </table>
          </div>

          <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Example Request</h4>
          <CodeBlock
            language="bash"
            code={`curl -X POST https://api.cosmicforge.io/api/v1/api/meetings \\
  -H "Content-Type: application/json" \\
  -H "Api-Key: cf_live_your_api_key" \\
  -d '{
    "title": "Product Demo",
    "description": "Weekly product demonstration",
    "is_private": false,
    "start_time": "2026-02-25T14:00:00Z",
    "end_time": "2026-02-25T15:00:00Z"
  }'`}
          />

          <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Example Response</h4>
          <CodeBlock
            language="json"
            code={`{
  "success": true,
  "data": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "meeting_identifier": "abc-defg-hij",
    "title": "Product Demo",
    "description": "Weekly product demonstration",
    "host_id": "user-uuid",
    "is_private": false,
    "status": "scheduled",
    "start_time": "2026-02-25T14:00:00Z",
    "end_time": "2026-02-25T15:00:00Z",
    "created_at": "2026-02-25T10:00:00Z",
    "updated_at": "2026-02-25T10:00:00Z"
  }
}`}
          />
        </CollapsibleSection>

        <CollapsibleSection title="POST /api/meetings/{id}/join - Join Meeting">
          <p className="text-sm text-gray-600 mb-4">
            Allows a participant to join an existing meeting.
          </p>

          <h4 className="font-medium text-sm text-gray-700 mb-2">Path Parameters</h4>
          <div className="overflow-x-auto">
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-50">
                  <th className="text-left p-2 border border-gray-200 font-medium">Parameter</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td className="p-2 border border-gray-200"><code>id</code></td>
                  <td className="p-2 border border-gray-200">Meeting ID (UUID) or meeting identifier</td>
                </tr>
              </tbody>
            </table>
          </div>

          <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Request Body</h4>
          <div className="overflow-x-auto">
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-50">
                  <th className="text-left p-2 border border-gray-200 font-medium">Field</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Type</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Required</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td className="p-2 border border-gray-200"><code>participant_name</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">Yes</td>
                  <td className="p-2 border border-gray-200">Display name for the participant</td>
                </tr>
              </tbody>
            </table>
          </div>

          <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Example Request</h4>
          <CodeBlock
            language="bash"
            code={`curl -X POST https://api.cosmicforge.io/api/v1/api/meetings/abc-defg-hij/join \\
  -H "Content-Type: application/json" \\
  -H "Api-Key: cf_live_your_api_key" \\
  -d '{
    "participant_name": "John Doe"
  }'`}
          />

          <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Example Response</h4>
          <CodeBlock
            language="json"
            code={`{
  "success": true,
  "data": {
    "participant_id": "participant-uuid",
    "meeting_id": "meeting-uuid",
    "join_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "websocket_url": "wss://api.cosmicforge.io/ws/meeting/abc-defg-hij"
  }
}`}
          />
        </CollapsibleSection>
      </section>

      {/* API Keys Management */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">API Keys Management</h3>
        <p className="text-sm text-gray-600">
          These endpoints require Bearer token authentication (JWT) and are used to manage your API keys.
          Access these via the{" "}
          <span
            className="font-medium text-[#029CD4] cursor-pointer hover:underline"
            onClick={() => router.push("/dashboard/settings")}
          >
            Settings Page
          </span>.
        </p>

        <CollapsibleSection title="POST /api-keys - Create API Key">
          <p className="text-sm text-gray-600 mb-4">
            Creates a new API key for third-party integrations.
          </p>

          <h4 className="font-medium text-sm text-gray-700 mb-2">Request Body</h4>
          <div className="overflow-x-auto">
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-50">
                  <th className="text-left p-2 border border-gray-200 font-medium">Field</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Type</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Required</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td className="p-2 border border-gray-200"><code>name</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">Yes</td>
                  <td className="p-2 border border-gray-200">A descriptive name for the API key</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div className="bg-blue-50 border border-blue-200 rounded-lg p-3 mt-4">
            <p className="text-sm text-blue-800">
              <strong>Note:</strong> The full API key is only shown once upon creation. Store it securely.
              Keys have a 100 request limit and expire after 20 days.
            </p>
          </div>
        </CollapsibleSection>

        <CollapsibleSection title="GET /api-keys - List API Keys">
          <p className="text-sm text-gray-600 mb-4">
            Returns all API keys for the authenticated user. The key values are masked for security.
          </p>
        </CollapsibleSection>

        <CollapsibleSection title="DELETE /api-keys/{id} - Delete API Key">
          <p className="text-sm text-gray-600 mb-4">
            Permanently deletes an API key. This action cannot be undone.
          </p>
        </CollapsibleSection>
      </section>

      {/* Error Responses */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Error Responses</h3>
        <div className="overflow-x-auto">
          <table className="w-full text-sm border-collapse">
            <thead>
              <tr className="bg-gray-50">
                <th className="text-left p-2 border border-gray-200 font-medium">Status Code</th>
                <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td className="p-2 border border-gray-200"><code>400</code></td>
                <td className="p-2 border border-gray-200">Bad Request - Invalid request body or parameters</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>401</code></td>
                <td className="p-2 border border-gray-200">Unauthorized - Missing or invalid API key</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>403</code></td>
                <td className="p-2 border border-gray-200">Forbidden - API key expired or rate limit exceeded</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>404</code></td>
                <td className="p-2 border border-gray-200">Not Found - Resource does not exist</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>500</code></td>
                <td className="p-2 border border-gray-200">Internal Server Error</td>
              </tr>
            </tbody>
          </table>
        </div>

        <CodeBlock
          language="json"
          code={`{
  "success": false,
  "error": {
    "code": "INVALID_API_KEY",
    "message": "The provided API key is invalid or expired"
  }
}`}
        />
      </section>
    </>
  );
}

function WebhooksTab({ router }: { router: ReturnType<typeof useRouter> }) {
  return (
    <>
      <section className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-lg bg-[#E6F5FA] flex items-center justify-center text-[#029CD4]">
            <Webhook className="w-5 h-5" />
          </div>
          <h2 className="text-xl font-bold text-[#343434]">
            Webhooks
          </h2>
        </div>
        <p className="pl-[52px] text-gray-600">
          Webhooks allow you to receive real-time HTTP notifications when events occur in your meetings.
          Configure webhooks in the{" "}
          <span
            className="font-medium text-[#029CD4] cursor-pointer hover:underline"
            onClick={() => router.push("/dashboard/settings")}
          >
            Settings Page
          </span>.
        </p>
      </section>

      {/* Event Types */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Event Types</h3>
        <div className="overflow-x-auto">
          <table className="w-full text-sm border-collapse">
            <thead>
              <tr className="bg-gray-50">
                <th className="text-left p-2 border border-gray-200 font-medium">Event</th>
                <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td className="p-2 border border-gray-200"><code>meeting_start</code></td>
                <td className="p-2 border border-gray-200">Triggered when a meeting begins</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>meeting_end</code></td>
                <td className="p-2 border border-gray-200">Triggered when a meeting ends</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>participant_join</code></td>
                <td className="p-2 border border-gray-200">Triggered when a participant joins a meeting</td>
              </tr>
              <tr>
                <td className="p-2 border border-gray-200"><code>participant_leave</code></td>
                <td className="p-2 border border-gray-200">Triggered when a participant leaves a meeting</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      {/* Webhook Payload */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Webhook Payload</h3>
        <p className="text-sm text-gray-600">
          When an event occurs, we send a POST request to your configured endpoint with a JSON payload.
        </p>

        <CollapsibleSection title="Meeting Start Event" defaultOpen>
          <CodeBlock
            language="json"
            code={`{
  "event": "meeting_start",
  "timestamp": "2026-02-25T14:00:00Z",
  "data": {
    "meeting_id": "550e8400-e29b-41d4-a716-446655440000",
    "meeting_identifier": "abc-defg-hij",
    "title": "Team Standup",
    "host_id": "user-uuid"
  }
}`}
          />
        </CollapsibleSection>

        <CollapsibleSection title="Meeting End Event">
          <CodeBlock
            language="json"
            code={`{
  "event": "meeting_end",
  "timestamp": "2026-02-25T15:00:00Z",
  "data": {
    "meeting_id": "550e8400-e29b-41d4-a716-446655440000",
    "meeting_identifier": "abc-defg-hij",
    "title": "Team Standup",
    "duration_seconds": 3600
  }
}`}
          />
        </CollapsibleSection>

        <CollapsibleSection title="Participant Join Event">
          <CodeBlock
            language="json"
            code={`{
  "event": "participant_join",
  "timestamp": "2026-02-25T14:05:00Z",
  "data": {
    "meeting_id": "550e8400-e29b-41d4-a716-446655440000",
    "meeting_identifier": "abc-defg-hij",
    "participant_id": "participant-uuid",
    "participant_name": "John Doe"
  }
}`}
          />
        </CollapsibleSection>

        <CollapsibleSection title="Participant Leave Event">
          <CodeBlock
            language="json"
            code={`{
  "event": "participant_leave",
  "timestamp": "2026-02-25T14:55:00Z",
  "data": {
    "meeting_id": "550e8400-e29b-41d4-a716-446655440000",
    "meeting_identifier": "abc-defg-hij",
    "participant_id": "participant-uuid",
    "participant_name": "John Doe"
  }
}`}
          />
        </CollapsibleSection>
      </section>

      {/* Signature Verification */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Signature Verification</h3>
        <p className="text-sm text-gray-600">
          All webhook requests include a signature in the <code className="px-1.5 py-0.5 bg-gray-100 rounded">X-Webhook-Signature</code> header.
          You should verify this signature to ensure the request came from CosmicForge.
        </p>

        <div className="bg-amber-50 border border-amber-200 rounded-xl p-4">
          <p className="text-sm text-amber-800">
            <strong>Important:</strong> Always verify webhook signatures in production to prevent
            unauthorized requests.
          </p>
        </div>

        <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">How it works</h4>
        <ol className="list-decimal list-inside text-sm text-gray-600 space-y-2">
          <li>When you create a webhook, you receive a unique <strong>secret key</strong></li>
          <li>Each webhook request includes an <code className="px-1.5 py-0.5 bg-gray-100 rounded">X-Webhook-Signature</code> header</li>
          <li>The signature is an HMAC-SHA256 hash of the request body using your secret key</li>
          <li>Compare the computed hash with the signature header to verify authenticity</li>
        </ol>

        <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Node.js Example</h4>
        <CodeBlock
          language="javascript"
          code={`const crypto = require('crypto');

function verifyWebhookSignature(payload, signature, secret) {
  const expectedSignature = crypto
    .createHmac('sha256', secret)
    .update(payload, 'utf8')
    .digest('hex');

  return crypto.timingSafeEqual(
    Buffer.from(signature),
    Buffer.from(expectedSignature)
  );
}

// Express.js middleware example
app.post('/webhook', express.raw({ type: 'application/json' }), (req, res) => {
  const signature = req.headers['x-webhook-signature'];
  const payload = req.body.toString();

  if (!verifyWebhookSignature(payload, signature, WEBHOOK_SECRET)) {
    return res.status(401).send('Invalid signature');
  }

  const event = JSON.parse(payload);
  console.log('Received event:', event.event);

  // Handle the event...

  res.status(200).send('OK');
});`}
        />

        <h4 className="font-medium text-sm text-gray-700 mt-4 mb-2">Python Example</h4>
        <CodeBlock
          language="python"
          code={`import hmac
import hashlib

def verify_webhook_signature(payload: bytes, signature: str, secret: str) -> bool:
    expected_signature = hmac.new(
        secret.encode('utf-8'),
        payload,
        hashlib.sha256
    ).hexdigest()

    return hmac.compare_digest(signature, expected_signature)

# Flask example
from flask import Flask, request

app = Flask(__name__)

@app.route('/webhook', methods=['POST'])
def webhook():
    signature = request.headers.get('X-Webhook-Signature')
    payload = request.get_data()

    if not verify_webhook_signature(payload, signature, WEBHOOK_SECRET):
        return 'Invalid signature', 401

    event = request.get_json()
    print(f"Received event: {event['event']}")

    # Handle the event...

    return 'OK', 200`}
        />
      </section>

      {/* Managing Webhooks */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Managing Webhooks</h3>
        <p className="text-sm text-gray-600">
          Use the Settings page to manage your webhooks, or use the API endpoints directly.
        </p>

        <CollapsibleSection title="POST /webhooks - Create Webhook">
          <p className="text-sm text-gray-600 mb-4">
            Creates a new webhook endpoint. Returns the webhook details including the secret key.
          </p>

          <h4 className="font-medium text-sm text-gray-700 mb-2">Request Body</h4>
          <div className="overflow-x-auto">
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-50">
                  <th className="text-left p-2 border border-gray-200 font-medium">Field</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Type</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Required</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td className="p-2 border border-gray-200"><code>endpoint_url</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">Yes</td>
                  <td className="p-2 border border-gray-200">Your HTTPS endpoint URL</td>
                </tr>
                <tr>
                  <td className="p-2 border border-gray-200"><code>event_type</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">Yes</td>
                  <td className="p-2 border border-gray-200">One of: meeting_start, meeting_end, participant_join, participant_leave</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div className="bg-blue-50 border border-blue-200 rounded-lg p-3 mt-4">
            <p className="text-sm text-blue-800">
              <strong>Note:</strong> The secret key is only returned once when creating the webhook.
              Store it securely for signature verification.
            </p>
          </div>
        </CollapsibleSection>

        <CollapsibleSection title="GET /webhooks - List Webhooks">
          <p className="text-sm text-gray-600 mb-4">
            Returns all webhooks for the authenticated user. The secret keys are not included in the response.
          </p>
        </CollapsibleSection>

        <CollapsibleSection title="PATCH /webhooks/{id} - Update Webhook">
          <p className="text-sm text-gray-600 mb-4">
            Updates an existing webhook. You can change the endpoint URL or status.
          </p>

          <h4 className="font-medium text-sm text-gray-700 mb-2">Request Body</h4>
          <div className="overflow-x-auto">
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-50">
                  <th className="text-left p-2 border border-gray-200 font-medium">Field</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Type</th>
                  <th className="text-left p-2 border border-gray-200 font-medium">Description</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td className="p-2 border border-gray-200"><code>endpoint_url</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">New endpoint URL</td>
                </tr>
                <tr>
                  <td className="p-2 border border-gray-200"><code>status</code></td>
                  <td className="p-2 border border-gray-200">string</td>
                  <td className="p-2 border border-gray-200">&quot;active&quot; or &quot;inactive&quot;</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div className="bg-gray-50 border border-gray-200 rounded-lg p-3 mt-4">
            <p className="text-sm text-gray-600">
              <strong>Note:</strong> The event type cannot be changed after creation.
              To change the event type, delete the webhook and create a new one.
            </p>
          </div>
        </CollapsibleSection>

        <CollapsibleSection title="DELETE /webhooks/{id} - Delete Webhook">
          <p className="text-sm text-gray-600 mb-4">
            Permanently deletes a webhook. You will stop receiving events for this endpoint.
          </p>
        </CollapsibleSection>
      </section>

      {/* Best Practices */}
      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-[#343434]">Best Practices</h3>
        <div className="space-y-3">
          <div className="flex gap-3 p-4 bg-gray-50 rounded-xl border border-gray-100">
            <ShieldCheck className="w-5 h-5 text-green-500 flex-shrink-0 mt-0.5" />
            <div>
              <h4 className="font-medium text-sm text-[#343434]">Always verify signatures</h4>
              <p className="text-sm text-gray-500">
                Use the X-Webhook-Signature header to verify that requests originate from CosmicForge.
              </p>
            </div>
          </div>
          <div className="flex gap-3 p-4 bg-gray-50 rounded-xl border border-gray-100">
            <Zap className="w-5 h-5 text-amber-500 flex-shrink-0 mt-0.5" />
            <div>
              <h4 className="font-medium text-sm text-[#343434]">Respond quickly</h4>
              <p className="text-sm text-gray-500">
                Return a 200 response within 30 seconds. Process events asynchronously if needed.
              </p>
            </div>
          </div>
          <div className="flex gap-3 p-4 bg-gray-50 rounded-xl border border-gray-100">
            <Server className="w-5 h-5 text-blue-500 flex-shrink-0 mt-0.5" />
            <div>
              <h4 className="font-medium text-sm text-[#343434]">Handle retries</h4>
              <p className="text-sm text-gray-500">
                Implement idempotency to handle potential duplicate webhook deliveries gracefully.
              </p>
            </div>
          </div>
          <div className="flex gap-3 p-4 bg-gray-50 rounded-xl border border-gray-100">
            <Key className="w-5 h-5 text-purple-500 flex-shrink-0 mt-0.5" />
            <div>
              <h4 className="font-medium text-sm text-[#343434]">Secure your secrets</h4>
              <p className="text-sm text-gray-500">
                Store webhook secrets in environment variables, not in code repositories.
              </p>
            </div>
          </div>
        </div>
      </section>
    </>
  );
}
