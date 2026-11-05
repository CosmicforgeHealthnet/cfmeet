"""CI stand-in for Messaging's realtime contract: status codes, error envelope,
single-use tickets and the WebSocket upgrade. It exercises the ingress path (host nginx,
relay, private TLS proxy), not Messaging; Messaging's own suites cover its sessions."""
import base64
import hashlib
import json
import secrets
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VALID_TOKEN = 'ci-valid-client-token'
SUBPROTOCOL = 'cfmeet.messaging.v1'
PREFIX = 'cfmeet.ticket.'
GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
TICKETS = set()


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def send(self, status, body=b'', headers=()):
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def refuse(self, status, code, reason=None):
        error = {'code': code, 'message': 'refused'}
        if reason:
            error['reason'] = reason
        headers = [('Content-Type', 'application/json'), ('Cache-Control', 'no-store')]
        if status == 401:
            headers.append(('WWW-Authenticate', 'Bearer realm="cf-meet-messaging"'))
        self.send(status, json.dumps({'error': error}).encode(), headers)

    def do_OPTIONS(self):
        # No browser origin is configured, so every preflight is refused.
        self.send(403 if self.path == '/v1/realtime/tickets' else 404)

    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length') or 0))
        if self.path != '/v1/realtime/tickets':
            return self.send(404)
        if self.headers.get('Origin'):
            return self.refuse(401, 'AUTHENTICATION_FAILED', 'ORIGIN_NOT_ALLOWED')
        if self.headers.get('Authorization') != 'CF-Client-Token ' + VALID_TOKEN:
            return self.refuse(401, 'AUTHENTICATION_FAILED')
        ticket = secrets.token_hex(32)
        TICKETS.add(ticket)
        body = json.dumps({'ticket': ticket, 'path': '/v1/realtime', 'subprotocol': SUBPROTOCOL,
                           'ticketSubprotocolPrefix': PREFIX}).encode()
        self.send(201, body, [('Content-Type', 'application/json')])

    def do_GET(self):
        if self.path == '/ready':
            return self.send(200)
        if self.path != '/v1/realtime':
            return self.send(404)
        if self.headers.get('X-Forwarded-Proto') != 'https':
            return self.refuse(403, 'AUTHENTICATION_FAILED', 'TLS_REQUIRED')
        offered = [p.strip() for p in (self.headers.get('Sec-WebSocket-Protocol') or '').split(',') if p.strip()]
        if SUBPROTOCOL not in offered:
            return self.refuse(400, 'VALIDATION_ERROR', 'SUBPROTOCOL_REQUIRED')
        found = [p[len(PREFIX):] for p in offered if p.startswith(PREFIX)]
        if len(found) != 1:
            return self.refuse(401, 'AUTHENTICATION_FAILED', 'TICKET_REQUIRED')
        if (self.headers.get('Upgrade') or '').lower() != 'websocket' \
                or 'upgrade' not in (self.headers.get('Connection') or '').lower():
            return self.refuse(426, 'VALIDATION_ERROR', 'WEBSOCKET_REQUIRED')
        if found[0] not in TICKETS:
            return self.refuse(401, 'AUTHENTICATION_FAILED', 'INVALID_TICKET')
        TICKETS.discard(found[0])
        accept = base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key'] + GUID).encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.send_header('Sec-WebSocket-Protocol', SUBPROTOCOL)
        self.end_headers()
        self.wfile.flush()
        # Echo one masked client text frame back unmasked, as a server must.
        head = self.rfile.read(2)
        length = head[1] & 0x7f
        mask = self.rfile.read(4)
        data = bytes(b ^ mask[i % 4] for i, b in enumerate(self.rfile.read(length)))
        self.wfile.write(bytes([0x81, len(data)]) + data)
        self.wfile.flush()
        self.close_connection = True


if __name__ == '__main__':
    ThreadingHTTPServer(('0.0.0.0', 8097), Handler).serve_forever()
