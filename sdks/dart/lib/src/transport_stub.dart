/// Platforms without `dart:io` (browsers): no default transport.
library;

import 'errors.dart';
import 'transport.dart';

/// Browser builds must pass a [CfMeetTransport] (e.g. over `package:http` / fetch).
CfMeetTransport createDefaultTransport() => throw const CfMeetUsageError(
    'no default HTTP transport on this platform: pass CfMeetClient(transport: ...)');
