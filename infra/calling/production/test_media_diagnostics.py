import unittest
import media_diagnostics as d

SECRET_JWT='eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzZXNzaW9uIn0.c2lnbmF0dXJlLXZhbHVlLXNlY3JldA'


class Redaction(unittest.TestCase):
    def assertClean(self,text):
        for fragment in (SECRET_JWT,'access_token','203.0.113.7','9f1b2c3d-1111-4222-8333-444455556666',
                         'PA_abcDEF123','wss://','s3cr3t-value-that-is-long-enough'):
            self.assertNotIn(fragment,text)

    def test_sdk_line_with_signed_url_and_addresses(self):
        line=('2026-11-05 08:14:06,123 INFO livekit connecting to wss://api.cfmeet.com/v1/calling/media/rtc?access_token='
              +SECRET_JWT+' from 203.0.113.7:51234 participant PA_abcDEF123 session 9f1b2c3d-1111-4222-8333-444455556666')
        out=d.template(line);self.assertClean(out)
        self.assertTrue(out.startswith('INFO livekit connecting to <url>'),out)

    def test_zap_console_line_drops_structured_fields(self):
        line=('2026-11-05T08:14:07.000000001Z 2026-11-05T08:14:07.001Z\tINFO\tlivekit\trtc/participant.go:412\tparticipant closing'
              '\t{"room":"call-9f1b2c3d-1111-4222-8333-444455556666","participant":"PA_abcDEF123","reason":"JOIN_TIMEOUT","ip":"203.0.113.7"}')
        out=d.template(line);self.assertClean(out)
        self.assertEqual(out,'INFO livekit participant closing')

    def test_json_tracing_line(self):
        line='2026-11-05T08:14:08Z {"level":"WARN","target":"calling::media","fields":{"message":"media gateway could not reach the media server","token":"'+SECRET_JWT+'"}}'
        out=d.template(line);self.assertClean(out)
        self.assertEqual(out,'WARN calling::media media gateway could not reach the media server')

    def test_nginx_error_drops_client_and_request(self):
        line=('2026/11/05 08:14:09 [error] 812#812: *4411 upstream timed out (110: Connection timed out) while reading response header '
              'from upstream, client: 203.0.113.7, server: api.cfmeet.com, request: "GET /v1/calling/media/rtc?access_token='+SECRET_JWT+' HTTP/1.1"')
        out=d.template(line);self.assertClean(out)
        self.assertEqual(out,'[error] upstream timed out (<n>: Connection timed out) while reading response header from upstream')

    def test_key_value_secrets_and_long_tokens(self):
        out=d.redact('password=s3cr3t-value-that-is-long-enough secret: abc bearer xyz '+'A'*40)
        self.assertClean(out);self.assertNotIn('abc',out);self.assertNotIn('xyz',out);self.assertNotIn('A'*30,out)

    def test_stamp_parses_supported_formats(self):
        for line in ('2026-11-05T08:14:06.123456789Z x','2026/11/05 08:14:06 [warn] x','2026-11-05 08:14:06,123 INFO x'):
            when,_=d.stamp(line)
            self.assertEqual((when.hour,when.minute,when.second),(8,14,6),line)

    def test_windows_are_well_formed_and_cover_all_failed_releases(self):
        failed={'41972828392','41974711025','41975667093','41977529784','41980193898','41981794294','41992435685'}
        self.assertTrue(failed<= {w[0] for w in d.WINDOWS})
        self.assertTrue(any(w[1]=='787bb9a' and w[2]=='FAIL' for w in d.WINDOWS))
        for run,image,outcome,start,end,_ in d.WINDOWS:
            self.assertLess(start,end);self.assertIn(outcome,('PASS','FAIL'))


if __name__=='__main__':
    unittest.main()
