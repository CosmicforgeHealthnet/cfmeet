-- Gap audit S9: "who can add me to groups" is its own privacy setting instead of reusing
-- the contact-request permission. Seeded from each profile's current contact-request
-- permission so nobody's effective setting changes; an older image ignores the column
-- (its INSERTs get the default).
ALTER TABLE communication_privacy ADD COLUMN IF NOT EXISTS group_invitation_policy TEXT;
UPDATE communication_privacy
   SET group_invitation_policy = CASE WHEN contact_request_permissions = 'NOBODY' THEN 'NOBODY' ELSE 'CONNECTIONS' END
 WHERE group_invitation_policy IS NULL;
ALTER TABLE communication_privacy ALTER COLUMN group_invitation_policy SET DEFAULT 'CONNECTIONS';
ALTER TABLE communication_privacy ALTER COLUMN group_invitation_policy SET NOT NULL;
ALTER TABLE communication_privacy ADD CONSTRAINT ck_communication_privacy_group_invitation_policy
    CHECK (group_invitation_policy IN ('CONNECTIONS', 'NOBODY'));
