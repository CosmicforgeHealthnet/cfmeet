import { CfMeetConfigurationError } from './errors.js';
import type { OperationSpec } from './generated/operations.js';
import type { RequestParams } from './types.js';

/** Builds `path` from the template; each parameter is checked and percent-encoded. */
export function buildPath(spec: Pick<OperationSpec, 'path' | 'pathParams'>, values: RequestParams['path'] = {}): string {
  for (const k of Object.keys(values)) {
    if (!spec.pathParams.includes(k)) throw new CfMeetConfigurationError(`unknown path parameter ${k} for ${spec.path}`);
  }
  return spec.path.replace(/\{([^}]+)\}/g, (_, name: string) => {
    const raw = values[name];
    if (raw === undefined || raw === null) throw new CfMeetConfigurationError(`missing path parameter ${name} for ${spec.path}`);
    if (typeof raw !== 'string' && typeof raw !== 'number') {
      throw new CfMeetConfigurationError(`path parameter ${name} must be a string or number`);
    }
    const v = String(raw);
    if (v === '' || v === '.' || v.includes('/') || v.includes('\\') || v.includes('..') || /[\u0000-\u001f\u007f]/.test(v)) {
      throw new CfMeetConfigurationError(`path parameter ${name} is empty or contains '/', '\\', '..' or control characters`);
    }
    return encodeURIComponent(v);
  });
}

