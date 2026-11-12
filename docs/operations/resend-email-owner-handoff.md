# Resend for CF Meet: owner handoff (`cfmeet.com`)

This is the single owner procedure for moving CF Meet's transactional email (OTP and verification codes, password resets, security notices, meeting and organization invitations, meeting notifications, Developer Platform decisions) from the rejected Gmail SMTP account to Resend.

Everything else is already built and deployed by the coordinated release:
- the durable email ledger;
- the Resend provider;
- signed webhooks;
- the verification step that switches providers only after a real accepted send;
- rollback to SMTP.

The owner steps below need your Resend account, your Hostinger DNS and your GitHub production environment. Nothing here touches the CF Meet website or any other DNS record.

> Never paste an API key or signing secret into chat, an issue, a commit or a log. The workflow takes them only from the GitHub `production` environment's secrets and writes them on the VPS as 0400 files.

## Current state (verified 2026-11-12, public DNS)

| Record | Today |
|---|---|
| Nameservers | `ns1.dns-parking.com`, `ns2.dns-parking.com` (Hostinger) |
| `cfmeet.com` A | `216.198.79.1` (the website; leave it alone) |
| `api.cfmeet.com` A | `173.249.30.55` (the VPS; leave it alone) |
| `cfmeet.com` MX / TXT (SPF) | none |
| `_dmarc.cfmeet.com` | none |
| `resend._domainkey.cfmeet.com`, `send.cfmeet.com` | none |

`cfmeet.com` has no MX record, so it receives no mail today. Outbound sending from `no-reply@cfmeet.com` through Resend needs no mailbox. Resend's records live on the `send` and `resend._domainkey` subdomains and do not create an inbox. If you later want replies or a real mailbox (for example `support@cfmeet.com`), set that up separately with a mail host; it is not required for this change.

## 1. Resend dashboard

1. Sign in at resend.com with the CosmicForge account that should own CF Meet mail.
2. Go to **Domains → Add Domain**, enter `cfmeet.com`, and pick the region closest to your users. The default `us-east-1` is fine; the region decides the MX value in step 2.
3. Resend shows the DNS records to create. Keep this page open.

## 2. Hostinger DNS (hPanel → Domains → cfmeet.com → DNS / Nameservers → DNS records)

Add **exactly the records Resend displays**. Copy each value from the Resend page; the DKIM key is unique to your domain and is not reproduced here. With the default settings they have this shape:

| Type | Name (host) in Hostinger | Value | Priority | TTL |
|---|---|---|---|---|
| MX | `send` | `feedback-smtp.<region>.amazonses.com` (copy from Resend) | 10 | default |
| TXT | `send` | `v=spf1 include:amazonses.com ~all` (copy from Resend) | — | default |
| TXT | `resend._domainkey` | `p=MIGfMA0GCSqGSIb3DQEB...` (the full DKIM public key from Resend) | — | default |
| TXT | `_dmarc` | `v=DMARC1; p=none;` (recommended start; tighten to `quarantine` after a few weeks of clean reports) | — | default |

Notes:
- In Hostinger, the **Name** field is relative to `cfmeet.com`. Enter `send`, not `send.cfmeet.com`.
- Do not add, change or delete the `@`/`www` A or CNAME records or any other record. The website and `api.cfmeet.com` must stay as they are.
- These records are on subdomains. They do not create or change MX for `cfmeet.com` itself.

## 3. Verify the domain

In Resend, click **Verify DNS Records** and wait until the domain shows **Verified**. DNS usually propagates in minutes but can take up to 24 hours. You can check from any machine:

```bash
dig +short TXT resend._domainkey.cfmeet.com
```

```bash
dig +short TXT send.cfmeet.com
```

```bash
dig +short MX send.cfmeet.com
```

Do not continue until Resend says **Verified**. Before that, Resend refuses to send from `@cfmeet.com`. The activation would then fail safely and keep SMTP, but OTP mail would still not arrive.

## 4. Dedicated API key

1. Go to Resend → **API Keys → Create API Key**.
2. Name it `cfmeet-production`.
3. Set **Permission** to `Sending access` and **Domain** to `cfmeet.com`.
4. Copy the key, which starts with `re_`. It is shown once.

Use a CF Meet key only. Do not reuse a key from another product such as Karryam.

## 5. Optional, recommended: delivery webhook

The webhook gives the backend **delivery evidence** (`DELIVERED`, `BOUNCED`, `COMPLAINED`) instead of only "accepted by Resend".

1. Go to Resend → **Webhooks → Add Endpoint**.
2. Set the URL to `https://api.cfmeet.com/api/v1/email/provider-events/resend`.
3. Select the events `email.sent`, `email.delivered`, `email.delivery_delayed`, `email.bounced`, `email.complained`, `email.failed`.
4. Copy the endpoint's **Signing Secret**, which starts with `whsec_`.

## 6. Secure placement (GitHub, not the VPS shell)

Go to GitHub → `CosmicforgeHealthnet/cfmeet` → **Settings → Environments → production**:

| Kind | Name | Value |
|---|---|---|
| Secret | `CFMEET_RESEND_API_KEY` | the `re_…` key |
| Secret | `CFMEET_RESEND_WEBHOOK_SECRET` | the `whsec_…` secret (if you did step 5) |
| Variable | `CFMEET_EMAIL_ACCEPTANCE_RECIPIENT` | a controlled inbox you can read, e.g. your own address. If unset, Resend's test inbox `delivered@resend.dev` is used, which proves acceptance and delivery but is not a human inbox. |

The workflow writes the secrets to `/opt/cfmeet-core/secrets/resend-api-key` and `/opt/cfmeet-core/secrets/resend-webhook-secret` (0400, root). It mounts them read-only into the Meeting Service as `RESEND_API_KEY_FILE` and `RESEND_WEBHOOK_SECRET_FILE`. They never appear in environment listings, logs or the release journal.

## 7. Activation (safe reload with automatic verification)

From GitHub Actions → **CF Meet coordinated production release** → **Run workflow** on `main`:

1. Set **operation** to `activate-resend-email` and **release_sha** to the current `main` head.

   The step does the following:
   - Installs the key and checks that the running Meeting Service has the email ledger.
   - Recreates **only** the Meeting Service with `EMAIL_PROVIDER=resend`, sender `CF Meet <no-reply@cfmeet.com>`.
   - Sends a controlled acceptance email to `CFMEET_EMAIL_ACCEPTANCE_RECIPIENT`.

   If Resend does not **accept** it (or, with the webhook configured, does not report it **delivered** within 3 minutes), the previous container is restored automatically and SMTP stays in place.

   The Gmail SMTP variables are left on the container, unused, so `rollback` can return to them.
2. **Controlled acceptance.**
   1. Confirm the email "CF Meet email delivery check acc-…" arrived in your inbox. Check spam the first time and mark it "not spam".
   2. Run **operation** `email-acceptance` with **confirm** `otp`. It runs the whole public OTP journey against production:
      - register a labelled acceptance account (`<inbox>+cfmeetotp…@…`);
      - Resend accepts the verification email;
      - the code is checked: a wrong code is refused, the right code verifies, a reused code is refused;
      - sign-in and an authenticated session;
      - the acceptance account is deactivated.

      You receive that verification email too.

Both runs print only statuses, the recipient's domain and the delivery id. They never print an address, a code or a body.

## 8. After activation

- The Gmail app password is no longer used. Once you are satisfied with Resend (a few days), revoke it in the Google account. A later Meeting Service configuration change can then drop the `SMTP_*` variables.
- Delivery status per message lives in the ledger. `diagnose` reports counts by status and category, and provider events.
- Resend's free plan limits are daily and monthly sends and about 2 requests per second. The worker rate-limits to 2 per second (`EMAIL_MAX_PER_SECOND`) and retries 429 responses. Watch the Resend dashboard if volume grows. Upgrading the plan is your decision; nothing here buys anything.

## Rollback

Run **operation** `rollback` with any SHA. It restores the Meeting Service container from before `activate-resend-email`, which brings back the SMTP transport. The ledger and queued messages are kept: messages queued while no working provider was configured are sent when one is.
