"""Acceptance transport recovery cannot hide authorization failures or replay writes."""
import ast
import asyncio
import io
import json
from pathlib import Path
import unittest
from unittest.mock import AsyncMock, patch
import urllib.error
import urllib.request
import uuid


def request_function():
    tree=ast.parse(Path(__file__).with_name('api_client.py').read_text())
    node=next(item for item in tree.body if isinstance(item,ast.AsyncFunctionDef) and item.name=='request')
    values=dict(asyncio=asyncio,json=json,urllib=urllib,uuid=uuid,TOKEN='private-fixture-token',REPORT={})
    exec(compile(ast.Module(body=[node],type_ignores=[]),'api_client.py','exec'),values)
    return values


class Response(io.BytesIO):
    status=200


class AcceptanceTransport(unittest.IsolatedAsyncioTestCase):
    async def test_get_recovers_once_without_exporting_reason_or_credentials(self):
        values=request_function()
        error=urllib.error.URLError('PRIVATE-REASON-MUST-NOT-LEAK')
        with patch('urllib.request.urlopen',side_effect=[error,Response(b'{"ok":true}')]) as send, \
             patch('asyncio.sleep',new_callable=AsyncMock):
            result=await values['request']('GET','/v1/calls/fixture')
        self.assertTrue(result['ok']);self.assertEqual(send.call_count,2)
        self.assertEqual(len(values['REPORT']['transport_failures']),1)
        self.assertNotIn('PRIVATE-REASON',json.dumps(values['REPORT']))
        self.assertNotIn(values['TOKEN'],json.dumps(values['REPORT']))

    async def test_http_authorization_denial_is_never_retried(self):
        values=request_function()
        denied=urllib.error.HTTPError('https://api.cfmeet.com',403,'denied',{},io.BytesIO(b'{"error":{"code":"INSUFFICIENT_SCOPE"}}'))
        with patch('urllib.request.urlopen',side_effect=denied) as send:
            with self.assertRaises(AssertionError):await values['request']('GET','/v1/calls/fixture')
        self.assertEqual(send.call_count,1)
        self.assertEqual(values['REPORT']['failed_request']['code'],'INSUFFICIENT_SCOPE')

    async def test_ambiguous_mutation_is_never_replayed(self):
        values=request_function()
        with patch('urllib.request.urlopen',side_effect=urllib.error.URLError('reset')) as send:
            with self.assertRaises(urllib.error.URLError):
                await values['request']('POST','/v1/calls',{'kind':'VOICE'})
        self.assertEqual(send.call_count,1)

    async def test_read_recovery_is_bounded_and_still_fails_on_persistent_outage(self):
        values=request_function()
        with patch('urllib.request.urlopen',side_effect=urllib.error.URLError('outage')) as send, \
             patch('asyncio.sleep',new_callable=AsyncMock):
            with self.assertRaises(urllib.error.URLError):await values['request']('GET','/v1/calls/fixture')
        self.assertEqual(send.call_count,3)
