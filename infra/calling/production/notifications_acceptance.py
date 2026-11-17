"""Notification Service production acceptance: real in-app delivery (authenticated, public API only).

Run on the VPS by `coordinated_release.py notifications-acceptance` (production lock, main head)
once Organizations publishes to the Notification Service (`activate-organizations-publications`)
and the `ORGANIZATION_UPDATES` IN_APP routing row is ACTIVE. Every request goes to
https://api.cfmeet.com exactly as a CF Meet client's would: real Organizations events, the real
Notification Service pipeline (I1 ingest -> X5 invitee resolution -> preferences -> inbox) and the
real Meeting relay U1-U9 (`/api/v2/notifications`). No database access, no operator route, no
injected event.

It reuses the organization acceptance journey's three fresh, email-verified accounts
(app_acceptance; secrets stay in 0600 files on the VPS):

  A  owner of org1 (inviter)      B  invitee / member of org1      C  outsider (owner of org2)

and proves, as named checks:
  - the invitee receives the invitation in its inbox while it is PENDING (event -> inbox);
  - unread count, read and dismiss work, and are idempotent;
  - another account sees nothing of it and cannot mark it read (isolation);
  - a preference switched off suppresses the next in-app notification of that category, and
    switched on again lets it through (preferences are enforced by the service);
  - a membership change reaches the member.

Cleanup (archiving both organizations, restoring B's preference) always runs. Output: one JSON
line per check (`notificationsAcceptance`) and per request (`organizationAcceptanceRequest`).
Never a token, email, password, name, title or body.
"""
import time

import organization_acceptance as oa

DELIVERY_WAIT_SECONDS = 150
QUIET_WAIT_SECONDS = 45
CATEGORY = 'organizationUpdates'

CHECKS = (
    'accounts_and_organizations',
    'invitee_receives_the_invitation_in_app',
    'unread_count_counts_it',
    'other_accounts_see_nothing_and_cannot_change_it',
    'read_and_dismiss_are_idempotent',
    'membership_change_reaches_the_member',
    'preference_off_suppresses_and_on_restores',
)


def is_org_item(item):
    return str(item.get('category', '')).replace('_', '').lower() in ('organizationupdates',)


class Journey(oa.Journey):
    """Reuses the organization journey's accounts, requests and cleanup; replaces its steps."""

    def __init__(self, accounts, emit, sleep=time.sleep):
        super().__init__(accounts, emit, sleep=sleep)
        self.n_checks = {}
        self.pref_changed = False

    def ncheck(self, name, ok=True):
        assert name in CHECKS and name not in self.n_checks, name
        self.n_checks[name] = 'PASS' if ok else 'FAIL'
        self.emit('notificationsAcceptance', {'check': name, 'result': self.n_checks[name]})
        if not ok:
            raise oa.Failed(name)

    # --- inbox helpers (counts and ids only) ---------------------------------------------------
    def inbox(self, person):
        _, page = self.call(person, 'GET', '/api/v2/notifications?limit=50')
        self.require(isinstance(page.get('items'), list), 'an inbox page')
        return page['items']

    def org_ids(self, person):
        return {i['id'] for i in self.inbox(person) if is_org_item(i)}

    def unread(self, person):
        _, doc = self.call(person, 'GET', '/api/v2/notifications/unread-count')
        value = doc.get('unreadCount', doc.get('count'))
        self.require(isinstance(value, int), 'an unread count')
        return value

    def wait_new(self, person, before, seconds=DELIVERY_WAIT_SECONDS):
        def probe():
            new = self.org_ids(person) - before
            return new or None
        return self.until(probe, seconds, 'no new in-app notification reached the account')

    def preferences(self, person):
        _, doc = self.call(person, 'GET', '/api/v2/notifications/preferences')
        self.require(isinstance(doc.get('version'), int), 'a preference document')
        return doc

    def set_category(self, person, enabled):
        doc = self.preferences(person)
        self.call(person, 'PUT', '/api/v2/notifications/preferences',
                  {'expectedVersion': doc['version'], 'categories': {CATEGORY: enabled}})
        self.require(self.preferences(person)['categories'][CATEGORY] is enabled, 'the preference is stored')

    # --- journey -----------------------------------------------------------------------------
    def run(self):
        try:
            for step in (self.setup, self.invitation_in_app, self.counts, self.isolation, self.read_dismiss,
                         self.membership_change, self.preference):
                self.step = step.__name__
                step()
        except Exception as error:
            failed = next(n for n in CHECKS if n not in self.n_checks)
            self.n_checks[failed] = 'FAIL'
            self.emit('notificationsAcceptance', {'check': failed, 'result': 'FAIL', 'failure': type(error).__name__,
                                                  'detail': str(error)[:200] if isinstance(error, oa.Failed) else ''})
        finally:
            self.step = 'cleanup'
            if self.pref_changed:
                try:
                    self.set_category(self.b, True)
                except Exception as error:  # recorded, never raised over the real failure
                    self.emit('notificationsAcceptance', {'check': 'cleanup-preference', 'result': 'FAIL',
                                                          'failure': type(error).__name__})
            for person, org in ((self.a, self.org1), (self.c, self.org2)):
                if org and org not in self.archived:
                    try:
                        self.call(person, 'POST', self.org_path(org, '/archive'), {}, expected=(200, 409))
                        self.archived.add(org)
                    except Exception as error:
                        self.emit('notificationsAcceptance', {'check': 'cleanup', 'result': 'FAIL',
                                                              'failure': type(error).__name__})
        for name in CHECKS:
            self.n_checks.setdefault(name, 'NOT_RUN')
        return self.n_checks

    def setup(self):
        self.accounts()
        self.create()
        self.provisioning()
        self.baseline = {p: self.org_ids(p) for p in (self.a, self.b, self.c)}
        self.ncheck('accounts_and_organizations')

    def invitation_in_app(self):
        self.unread_before = self.unread(self.b)
        _, inv = self.call(self.a, 'POST', self.org_path(self.org1, '/invitations'),
                           {'email': self.b.account['email'], 'role': 'MEMBER'}, expected=(201,))
        self.token = inv.get('token')
        self.require(self.token and inv.get('status') == 'PENDING', 'a PENDING invitation')
        # The service delivers an invitation only while it is PENDING: accept after it arrived.
        new = self.wait_new(self.b, self.baseline[self.b])
        self.require(len(new) == 1, 'exactly one invitation notification (no duplicate)')
        self.invite_item = next(iter(new))
        self.ncheck('invitee_receives_the_invitation_in_app')

    def counts(self):
        self.require(self.unread(self.b) == self.unread_before + 1, 'the unread count rose by one')
        self.ncheck('unread_count_counts_it')

    def isolation(self):
        self.require(not (self.org_ids(self.c) - self.baseline[self.c]), 'the outsider received nothing')
        # The outsider names B's item: nothing of B's changes.
        self.call(self.c, 'POST', '/api/v2/notifications/read', {'ids': [self.invite_item]},
                  expected=(200, 204, 404))
        self.require(self.unread(self.b) == self.unread_before + 1, "B's item is still unread")
        self.ncheck('other_accounts_see_nothing_and_cannot_change_it')

    def read_dismiss(self):
        for _ in range(2):  # idempotent
            self.call(self.b, 'POST', '/api/v2/notifications/read', {'ids': [self.invite_item]}, expected=(200, 204))
        self.require(self.unread(self.b) == self.unread_before, 'read lowers the unread count once')
        for _ in range(2):
            self.call(self.b, 'POST', '/api/v2/notifications/dismiss', {'ids': [self.invite_item]},
                      expected=(200, 204))
        self.require(self.invite_item not in self.org_ids(self.b), 'a dismissed item leaves the inbox')
        # B accepts now (the membership the next checks change).
        _, membership = self.call(self.b, 'POST', '/api/v2/organization-invitations/accept', {'token': self.token})
        self.require(membership.get('status') == 'ACTIVE', 'B is a member')
        self.b.membership = membership
        self.ncheck('read_and_dismiss_are_idempotent')

    def change_role(self, role):
        m = self.b.membership
        _, updated = self.call(self.a, 'PATCH', self.org_path(self.org1, '/members/' + m['id']),
                               {'expectedVersion': m['version'], 'role': role})
        self.b.membership = updated if isinstance(updated, dict) and updated.get('version') else m

    def membership_change(self):
        before = self.org_ids(self.b)
        self.change_role('ADMIN')
        self.wait_new(self.b, before)
        self.ncheck('membership_change_reaches_the_member')

    def preference(self):
        self.set_category(self.b, False)
        self.pref_changed = True
        before = self.org_ids(self.b)
        self.change_role('MEMBER')
        self.sleep(QUIET_WAIT_SECONDS)
        self.require(not (self.org_ids(self.b) - before), 'nothing delivered while the category is off')
        self.set_category(self.b, True)
        self.pref_changed = False
        self.change_role('ADMIN')
        self.wait_new(self.b, before)
        self.ncheck('preference_off_suppresses_and_on_restores')


def run(run_id, emit):
    import app_acceptance
    accounts = app_acceptance.accounts('notifications-' + run_id.replace('_', '-').lower()[:48], count=3,
                                      security_checks=True)
    return Journey(accounts, emit).run()
