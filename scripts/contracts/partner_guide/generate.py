"""Generate docs/platform/partner-api-frontend-guide.md from the published OpenAPI contracts.

    python3 scripts/contracts/partner_guide/generate.py

guide_head.md is the hand-written part (status, flows); endpoints.json selects the operations
(Parts 1-3) in order. Everything below the head is generated: edit the inputs, not the output."""
import os
import json
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
repo = os.path.abspath(os.path.join(HERE, '..', '..', '..'))
out_path = os.path.join(repo, 'docs', 'platform', 'partner-api-frontend-guide.md')
A = json.load(open(repo + '/packages/contracts/bundled/public/aggregate/v1.openapi.json'))
M = json.load(open(repo + '/packages/contracts/openapi/meeting-service.json'))
SPECS = {'A': A, 'M': M}

AUTH = {
    'developer_platform.userSession': 'Developer session — the signed-in CF Meet user\'s access token (`Authorization: Bearer <CF Meet session>`)',
    'bearer_auth': 'CF Meet session — the signed-in user\'s access token (`Authorization: Bearer …`)',
    'meetings.bearer_auth': 'CF Meet session — the signed-in user\'s access token (`Authorization: Bearer …`)',
    'api_key_auth': 'API key',
    'participant_token': 'Participant token (`X-Participant-Token` header, returned by join)',
    'meetings.participant_token': 'Participant token (`X-Participant-Token` header, returned by join)',
    'platformCredential': 'Platform credential `cfmk_…` (partner SERVER only, never in a browser)',
    'developer_platform.platformCredential': 'Platform credential `cfmk_…` (partner SERVER only, never in a browser)',
    'calling.platformCredential': 'Platform credential `cfmk_…` (partner SERVER only)',
    'meetings.platformCredential': 'Platform credential `cfmk_…` (partner SERVER only)',
    'messaging.customerCredential': 'Platform credential `cfmk_…` (partner SERVER only)',
    'gateway.platformCredential': 'Platform credential `cfmk_…` (partner SERVER only)',
    'userToken': 'User token `cfut_…` (minted by the partner server for one user; used on the device)',
    'meetings.userToken': 'User token `cfut_…` (minted by the partner server for one user; used on the device)',
    'messaging.userToken': 'User token `cfut_…` (minted by the partner server for one user; used on the device)',
    'calling.userToken': 'User token `cfut_…` (minted by the partner server for one user; used on the device)',
    'messaging.clientToken': 'Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device)',
    'calling.signalingCredential': 'Signaling credential `cfsig_…` (from the calling user-session exchange; device)',
    'oauthAccessToken': 'OAuth access token `cfoa_…` (a CF Meet App acting for a user)',
    'meetings.oauthAccessToken': 'OAuth access token `cfoa_…` (a CF Meet App acting for a user)',
    'messaging.oauthAccessToken': 'OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)',
    'messaging.realtimeTicket': 'Realtime ticket (WebSocket subprotocol `cfmeet.messaging.v1, cfmeet.ticket.<ticket>`)',
}
AUTH_MODE = {
    'DEVELOPER_SESSION': 'Developer session (signed-in CF Meet user)',
    'PLATFORM_CREDENTIAL': 'Platform credential `cfmk_…` (partner SERVER only)',
    'USER_TOKEN': 'User token `cfut_…` (device)',
    'CLIENT_TOKEN': 'Client token `CF-Client-Token …` (device)',
    'SIGNALING_CREDENTIAL': 'Signaling credential `cfsig_…` (device)',
    'OAUTH_ACCESS_TOKEN': 'OAuth access token `cfoa_…`',
    'PUBLIC': 'None (public)',
    'ANONYMOUS': 'None (public)',
    'PARTICIPANT_TOKEN': 'Participant token (`X-Participant-Token`)',
    'REALTIME_TICKET': 'Realtime ticket (WebSocket)',
    'CLIENT_SECRET_BASIC': 'App client credentials (`Authorization: Basic base64(clientId:clientSecret)`)',
}

# ---------------------------------------------------------------- type naming
names = {}      # (spec, ref) -> TS name
taken = {}      # TS name -> (spec, ref)
order = []      # (spec, ref) in first-use order
pending = []


def short(ref):
    return re.sub(r'[^A-Za-z0-9_]', '_', ref.split('/')[-1].split('.')[-1])


def type_name(spec, ref):
    key = (spec, ref)
    if key in names:
        return names[key]
    base = short(ref)
    name = base
    if name in taken and taken[name] != key:
        parts = ref.split('/')[-1].split('.')
        if len(parts) == 1:
            prefix = {'A': 'Gateway', 'M': 'MeetingService'}[spec]
        else:
            prefix = ''.join(p[:1].upper() + p[1:] for p in re.split(r'[_\-]', parts[0]))
        name = prefix + base
        n = 2
        while name in taken and taken[name] != key:
            name = '%s%s%d' % (prefix, base, n)
            n += 1
    names[key] = name
    taken[name] = key
    order.append(key)
    pending.append(key)
    return name


def deref(spec, obj, kind=None):
    seen = 0
    while isinstance(obj, dict) and '$ref' in obj and seen < 10:
        parts = obj['$ref'].lstrip('#/').split('/')
        cur = SPECS[spec]
        for p in parts:
            cur = cur[p]
        obj = cur
        seen += 1
    return obj


def is_named(obj):
    return isinstance(obj, dict) and '$ref' in obj and '/schemas/' in obj['$ref']


def fmt_doc(text, limit=400):
    text = re.sub(r'\s+', ' ', (text or '').strip())
    return text if len(text) <= limit else text[:limit].rsplit(' ', 1)[0] + ' …'


def ts(spec, s, indent=0):
    if s is None:
        return 'unknown'
    if is_named(s):
        return type_name(spec, s['$ref'])
    if '$ref' in s:
        return ts(spec, deref(spec, s), indent)
    def constraint_only(x):
        return isinstance(x, dict) and not any(k in x for k in ('$ref', 'type', 'properties', 'items'))
    if 'allOf' in s and (s.get('type') or s.get('properties')) and all(constraint_only(x) for x in s['allOf']):
        s = {k: v for k, v in s.items() if k != 'allOf'}  # validation-only rules (if/then/not)
    nullable = s.get('nullable')
    t = s.get('type')
    if isinstance(t, list):
        nullable = nullable or 'null' in t
        t = [x for x in t if x != 'null']
        t = t[0] if len(t) == 1 else t
    if 'enum' in s:
        out = ' | '.join(json.dumps(v) for v in s['enum'] if v is not None)
        nullable = nullable or None in s['enum']
    elif 'const' in s:
        out = json.dumps(s['const'])
    elif s.get('properties') and any(k in s for k in ('oneOf', 'anyOf')) and all(
            isinstance(x, dict) and '$ref' not in x and 'type' not in x
            for x in (s.get('oneOf') or s.get('anyOf'))):
        # Fields live in `properties`; oneOf/anyOf only constrain which combinations are valid.
        combos = ['[' + ', '.join(x.get('required') or []) + ']' for x in (s.get('oneOf') or s.get('anyOf'))]
        out = obj_body(spec, s, indent)
        out = out[:-1].rstrip() + '\n' + '  ' * (indent + 1) + '// exactly one of these field sets: ' + ' | '.join(combos) + '\n' + '  ' * indent + '}'
    elif 'allOf' in s:
        parts = [ts(spec, x, indent) for x in s['allOf']]
        out = parts[0] if len(parts) == 1 else ' & '.join(parts)
    elif 'oneOf' in s or 'anyOf' in s:
        alts = s.get('oneOf') or s.get('anyOf')
        parts = []
        for x in alts:
            if isinstance(x, dict) and x.get('type') == 'null':
                nullable = True
                continue
            parts.append(ts(spec, x, indent))
        out = ' | '.join(dict.fromkeys(parts)) or 'unknown'
    elif t == 'array':
        inner = ts(spec, s.get('items'), indent)
        out = ('(%s)[]' if ('|' in inner or '&' in inner) else '%s[]') % inner
    elif t == 'object' or 'properties' in s:
        props = s.get('properties') or {}
        ap = s.get('additionalProperties')
        if not props:
            if isinstance(ap, dict):
                out = 'Record<string, %s>' % ts(spec, ap, indent)
            else:
                out = 'Record<string, unknown>'
        else:
            out = obj_body(spec, s, indent)
    elif t in ('integer', 'number'):
        out = 'number'
    elif t == 'boolean':
        out = 'boolean'
    elif t == 'string':
        out = 'string'
    elif t == 'null':
        out, nullable = 'null', False
    else:
        out = 'unknown'
    return out + (' | null' if nullable and not out.endswith('| null') else '')


def obj_body(spec, s, indent):
    pad = '  ' * (indent + 1)
    req = set(s.get('required') or [])
    lines = ['{']
    for k, v in (s.get('properties') or {}).items():
        vd = deref(spec, v) if not is_named(v) else {}
        doc = fmt_doc((v.get('description') if isinstance(v, dict) else '') or vd.get('description'), 300)
        fmt = (v.get('format') or vd.get('format')) if isinstance(v, dict) else None
        extra = []
        if fmt:
            extra.append(fmt)
        for key in ('minLength', 'maxLength', 'minimum', 'maximum', 'maxItems', 'pattern', 'default'):
            val = v.get(key) if isinstance(v, dict) else None
            if val is None:
                val = vd.get(key)
            if val is not None:
                extra.append('%s: %s' % (key, json.dumps(val)))
        if v.get('readOnly') if isinstance(v, dict) else False:
            extra.append('read-only')
        note = ' '.join(x for x in [doc, '(' + '; '.join(extra) + ')' if extra else ''] if x)
        if note:
            lines.append('%s/** %s */' % (pad, note.replace('*/', '*\\/')))
        key = k if re.fullmatch(r'[A-Za-z_$][A-Za-z0-9_$]*', k) else json.dumps(k)
        lines.append('%s%s%s: %s;' % (pad, key, '' if k in req else '?', ts(spec, v, indent + 1)))
    ap = s.get('additionalProperties')
    if isinstance(ap, dict):
        lines.append('%s[key: string]: %s;' % (pad, ts(spec, ap, indent + 1)))
    lines.append('  ' * indent + '}')
    return '\n'.join(lines)


# ---------------------------------------------------------------- operations
def auth_of(spec, op):
    modes = op.get('x-cfmeet-auth')
    labels = []
    for req in op.get('security', []) or []:
        if not req:
            labels.append('None (public)')
        for k in req:
            labels.append(AUTH.get(k, k))
    if not labels and modes:
        labels = [AUTH_MODE.get(m, m) for m in modes]
    if not labels:
        labels = ['None (public)'] if op.get('security') == [] else ['see description']
    return list(dict.fromkeys(labels))


def media_schema(spec, body):
    body = deref(spec, body)
    content = (body or {}).get('content') or {}
    for mt in ('application/json', 'application/x-www-form-urlencoded', 'multipart/form-data'):
        if mt in content:
            return mt, content[mt].get('schema')
    for mt, v in content.items():
        return mt, v.get('schema')
    return None, None


def render_op(spec, method, path, section_notes=None):
    ops = SPECS[spec]['paths'].get(path)
    if not ops or method.lower() not in ops:
        raise SystemExit('missing %s %s in %s' % (method, path, spec))
    op = ops[method.lower()]
    out = []
    summary = fmt_doc(op.get('summary') or op.get('operationId') or '', 200)
    out.append('#### `%s %s`' % (method.upper(), path))
    out.append('')
    if summary:
        out.append('**%s**' % summary.rstrip('.'))
        out.append('')
    desc = fmt_doc(op.get('description') or '', 4000)
    head = summary.rstrip('.').rstrip(' …')
    if head and desc.startswith(head):
        desc = desc[len(head):].lstrip('.').strip()
    if desc:
        out.append(fmt_doc(desc, 1500))
        out.append('')
    out.append('- **Auth:** ' + ' **or** '.join(auth_of(spec, op)))
    if op.get('operationId'):
        out.append('- **operationId:** `%s`' % op['operationId'])
    params = [deref(spec, p) for p in (ops.get('parameters') or []) + (op.get('parameters') or [])]
    if params:
        out.append('- **Parameters:**')
        out.append('')
        out.append('  | Name | In | Type | Required | Description |')
        out.append('  |---|---|---|---|---|')
        for p in params:
            typ = ts(spec, p.get('schema')).replace('\n', ' ').replace('|', '\\|')
            out.append('  | `%s` | %s | `%s` | %s | %s |' % (
                p.get('name'), p.get('in'), typ if len(typ) < 80 else typ[:77] + '…',
                'yes' if p.get('required') else 'no', fmt_doc(p.get('description'), 260).replace('|', '\\|')))
        out.append('')
    if op.get('requestBody'):
        mt, sch = media_schema(spec, op['requestBody'])
        rb = deref(spec, op['requestBody'])
        out.append('- **Request body** (`%s`%s): `%s`' % (mt, ', required' if rb.get('required') else '',
                                                         ts(spec, sch).replace('\n', ' ') if is_named(sch) else 'inline, below'))
        if sch is not None and not is_named(sch):
            out.append('')
            out.append('  ```ts')
            out.append('  ' + ts(spec, sch, 1).replace('\n', '\n  '))
            out.append('  ```')
        out.append('')
    out.append('- **Responses:**')
    out.append('')
    errors = {}
    for code, resp in (op.get('responses') or {}).items():
        r = deref(spec, resp)
        mt, sch = media_schema(spec, r)
        typ = ts(spec, sch) if sch is not None else None
        if str(code)[0] in '45':
            errors.setdefault(typ or '—', []).append(code)
            continue
        line = '  - `%s` %s' % (code, fmt_doc(r.get('description'), 220))
        if typ and is_named(sch):
            line += ' → `%s`' % typ
        out.append(line)
        if typ and sch is not None and not is_named(sch):
            out.append('')
            out.append('    ```ts')
            out.append('    ' + ts(spec, sch, 2).replace('\n', '\n    '))
            out.append('    ```')
    for typ, codes in errors.items():
        out.append('  - Errors %s%s' % (', '.join('`%s`' % c for c in codes),
                                          (' → `%s`' % typ) if typ != '—' else ''))
    out.append('')
    return '\n'.join(out)


GROUPS = json.load(open(os.path.join(HERE, 'endpoints.json')))
doc = [open(os.path.join(HERE, 'guide_head.md')).read()]
for group in GROUPS:
    doc.append(group['heading'])
    doc.append('')
    if group.get('intro'):
        doc.append(group['intro'])
        doc.append('')
    for spec, method, path in group.get('ops', []):
        doc.append(render_op(spec, method, path))
    if group.get('index'):
        doc.append('| Method | Path | Auth | Summary |')
        doc.append('|---|---|---|---|')
        for spec, method, path in group['index']:
            op = SPECS[spec]['paths'][path][method.lower()]
            doc.append('| `%s` | `%s` | %s | %s |' % (method.upper(), path, ' / '.join(
                AUTH_MODE.get(m, m) for m in op.get('x-cfmeet-auth') or []), fmt_doc(op.get('summary'), 120)))
        doc.append('')

# types appendix: render every referenced schema (rendering can discover more)
types = []
done = set()
while pending:
    key = pending.pop(0)
    if key in done:
        continue
    done.add(key)
    spec, ref = key
    s = deref(spec, {'$ref': ref})
    name = names[key]
    d = fmt_doc(s.get('description'), 500)
    body = ts(spec, dict(s, nullable=False) if isinstance(s, dict) else s, 0)
    block = []
    if d:
        block.append('/** %s */' % d.replace('*/', '*\\/'))
    if body.startswith('{'):
        block.append('export interface %s %s' % (name, body))
    else:
        block.append('export type %s = %s;' % (name, body))
    types.append((name, '\n'.join(block)))
types.sort(key=lambda x: x[0].lower())
doc.append('## Appendix A — TypeScript types (generated from the contracts)')
doc.append('')
doc.append('Every request and response type referenced above, once. Field comments are the contract descriptions.')
doc.append('')
doc.append('```ts')
doc.append('\n\n'.join(b for _, b in types))
doc.append('```')
open(out_path, 'w').write('\n'.join(doc) + '\n')
print('ops', sum(len(g.get('ops', [])) for g in GROUPS), 'index', sum(len(g.get('index', [])) for g in GROUPS), 'types', len(types))
