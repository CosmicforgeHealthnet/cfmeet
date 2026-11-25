"""Keep native documentation aligned with the actual Rust relay and public bundle."""
import unittest

import generate_consumer_guide as guide


class NativeGuide(unittest.TestCase):
    def test_bounded_history_chunk_routes_are_documented_as_native(self):
        routes, _ = guide.mappings(guide.RELAY.read_text(encoding='utf-8'))
        path = '/v1/messaging/history-transfers-v2/{transferId}/chunks/{chunkIndex}'
        self.assertIn(('get', guide.shape(path)), routes)
        self.assertIn(('put', guide.shape(path)), routes)
        self.assertNotIn(('delete', guide.shape(path)), routes)
        rendered = guide.render()
        self.assertIn('Canonical SDK contract: **123 operations**', rendered)
        self.assertIn('**118 operations**; native Account transports: **85 operations**', rendered)
        for method in ('GET', 'PUT'):
            row = next(line for line in rendered.splitlines() if line.startswith(f'| {method} | `{path}` |'))
            self.assertIn(f'`/api/v2/messaging/relay{path}`', row)
            self.assertNotIn('Not exposed', row)


if __name__ == '__main__':
    unittest.main()
