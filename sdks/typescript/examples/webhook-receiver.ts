// Server-side: receive CF Meet webhooks with Node's http module.
//
//   CFMEET_WEBHOOK_SECRET=whsec_... node webhook-receiver.js
//
// Verify the RAW body (never re-serialized JSON), answer 2xx quickly, and dedupe on the
// event id (CF-Webhook-Event-Id): retries and replays deliver the same event again.

import { createServer } from 'node:http';

import { CfMeetWebhookSignatureError, constructEvent } from '@cfmeet/platform-sdk';

const secret = process.env.CFMEET_WEBHOOK_SECRET ?? '';
const seen = new Set<string>(); // use a durable store in production

const server = createServer((req, res) => {
  if (req.method !== 'POST' || req.url !== '/cfmeet/webhooks') {
    res.writeHead(404).end();
    return;
  }
  const chunks: Buffer[] = [];
  let size = 0;
  req.on('data', (c: Buffer) => {
    size += c.length;
    if (size > 1_000_000) req.destroy();
    else chunks.push(c);
  });
  req.on('end', async () => {
    try {
      const event = await constructEvent({ secret, headers: req.headers, rawBody: Buffer.concat(chunks) });
      const key = event.eventId ?? event.id;
      if (!seen.has(key)) {
        seen.add(key);
        console.log(`${event.type} ${JSON.stringify(event.data)} (delivery ${event.deliveryId})`);
      }
      res.writeHead(204).end();
    } catch (e) {
      if (e instanceof CfMeetWebhookSignatureError) {
        res.writeHead(400).end(); // never reveal why
        return;
      }
      res.writeHead(500).end();
    }
  });
});

server.listen(Number(process.env.PORT ?? 8787));
