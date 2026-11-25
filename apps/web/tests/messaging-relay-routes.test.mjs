import assert from "node:assert/strict";
import test from "node:test";
import { isMessagingRelayRoute } from "../src/lib/messaging/routes.ts";

const id = "11111111-1111-4111-8111-111111111111";
const transfer = `v1/messaging/history-transfers-v2/${id}`;

test("device discovery permits complete pagination and v2 recovery routes", () => {
  for (const [method, path] of [
    ["GET", `v1/messaging/principals/${id}/devices?limit=500&cursor=${id}`],
    ["GET", "v1/messaging/history-transfer-capabilities"],
    ["GET", `v1/messaging/history-transfers-v2?deviceId=${id}`],
    ["POST", "v1/messaging/history-transfers-v2"], ["GET", transfer],
    ["GET", `${transfer}/manifest`], ["PUT", `${transfer}/manifest`],
    ["GET", `${transfer}/chunks/0`], ["PUT", `${transfer}/chunks/4095`],
    ["POST", `${transfer}/checkpoint`], ["POST", `${transfer}/complete`], ["POST", `${transfer}/cancel`],
  ]) assert.equal(isMessagingRelayRoute(method, path), true, `${method} ${path}`);
});

test("new relay routes retain numeric bounds, method and authority restrictions", () => {
  for (const [method, path] of [
    ["GET", `${transfer}/chunks/4096`], ["GET", `${transfer}/chunks/-1`],
    ["GET", `${transfer}/chunks/00`], ["GET", `${transfer}/chunks/0%2fadmin`],
    ["GET", `${transfer}/manifest?principalId=${id}`], ["POST", `${transfer}/manifest`],
    ["GET", `v1/messaging/principals/${id}/devices?tenantId=${id}`],
    ["GET", `${transfer}/manifest#ignored`], ["GET", `v1/internal/history-transfers-v2/${id}`],
  ]) assert.equal(isMessagingRelayRoute(method, path), false, `${method} ${path}`);
});
