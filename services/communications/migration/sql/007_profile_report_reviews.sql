-- Gap audit S4: operator review of profile reports. A review is a separate row (reports
-- stay as filed); it goes with its report at retention.
CREATE TABLE IF NOT EXISTS communication_profile_report_reviews (
 report_id UUID PRIMARY KEY REFERENCES communication_profile_reports(id) ON DELETE CASCADE,
 decision TEXT NOT NULL CHECK (decision IN ('DISMISSED','ACTIONED')),
 reason TEXT NOT NULL CHECK (reason IN ('NO_VIOLATION','ABUSE','SECURITY','LEGAL','OTHER')),
 held BOOLEAN NOT NULL DEFAULT false,
 reviewed_by TEXT NOT NULL CHECK (length(reviewed_by) BETWEEN 1 AND 128),
 reviewed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
