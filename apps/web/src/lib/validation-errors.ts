/**
 * F1: field-level problems of a `400 VALIDATION_ERROR` answer.
 *
 * The Meeting Service lists one `{ field, code, message }` per problem under
 * `error.details`, naming the public JSON field (`password`, `confirm_password`, …);
 * `error.message` keeps the older `field: message` lines. Forms map the issues onto
 * their own field keys and fall back to the message (without the `field: ` prefix)
 * when an older server sends no details.
 */

export interface ValidationIssue {
  field: string | null;
  code: string;
  message: string;
}

type ErrorBody = { code?: unknown; message?: unknown; details?: unknown };

function errorBodyOf(error: unknown): ErrorBody | null {
  const body = (error as { response?: { data?: { error?: unknown } } } | null)?.response?.data?.error;
  return body && typeof body === "object" ? (body as ErrorBody) : null;
}

/** The structured issues of a `VALIDATION_ERROR` answer (empty for anything else). */
export function validationIssuesOf(error: unknown): ValidationIssue[] {
  const body = errorBodyOf(error);
  if (body?.code !== "VALIDATION_ERROR" || !Array.isArray(body.details)) return [];
  return body.details.flatMap((d): ValidationIssue[] => {
    const issue = d as { field?: unknown; code?: unknown; message?: unknown } | null;
    if (!issue || typeof issue.message !== "string" || !issue.message) return [];
    return [{
      field: typeof issue.field === "string" ? issue.field : null,
      code: typeof issue.code === "string" ? issue.code : "invalid",
      message: issue.message,
    }];
  });
}

/** `"password: Too short\nemail: Invalid"` → `"Too short Invalid"`; other text unchanged. */
export function stripFieldPrefixes(message: string): string {
  return message
    .split("\n")
    .map((line) => line.replace(/^[A-Za-z_][\w.[\]]*:\s+/, "").trim())
    .filter(Boolean)
    .join(" ");
}

/**
 * Form errors from a failed request: each issue whose API field is in `fields` goes to
 * that form key (first message wins); every other issue, or the stripped message when
 * there are no issues, goes to `generalKey`. Empty when the answer is not a
 * `VALIDATION_ERROR`.
 */
export function formErrorsOf(
  error: unknown,
  fields: Record<string, string>,
  generalKey = "general",
): Record<string, string> {
  const body = errorBodyOf(error);
  if (body?.code !== "VALIDATION_ERROR") return {};
  const issues = validationIssuesOf(error);
  if (issues.length === 0) {
    return typeof body.message === "string" && body.message
      ? { [generalKey]: stripFieldPrefixes(body.message) }
      : {};
  }
  const out: Record<string, string> = {};
  const general: string[] = [];
  for (const issue of issues) {
    const key = issue.field !== null ? fields[issue.field] : undefined;
    if (key === undefined) {
      if (!general.includes(issue.message)) general.push(issue.message);
    } else if (!out[key]) {
      out[key] = issue.message;
    }
  }
  if (general.length > 0) out[generalKey] = general.join(" ");
  return out;
}

/**
 * One line for a form that shows a single error: the issue messages (deduplicated), or
 * the message without `field: ` prefixes; `null` when the answer is not a
 * `VALIDATION_ERROR`.
 */
export function validationMessageOf(error: unknown): string | null {
  const body = errorBodyOf(error);
  if (body?.code !== "VALIDATION_ERROR") return null;
  const messages = [...new Set(validationIssuesOf(error).map((i) => i.message))];
  if (messages.length > 0) return messages.join(" ");
  return typeof body.message === "string" && body.message ? stripFieldPrefixes(body.message) : null;
}
