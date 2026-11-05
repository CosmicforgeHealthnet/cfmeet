"""TLS setup recovery cannot hide denials or replay ambiguous writes."""
import ast
import asyncio
import http.client
import io
import json
from pathlib import Path
import ssl
import unittest
from unittest.mock import AsyncMock, MagicMock, patch
import urllib.error
import uuid


def request_function():
    tree=ast.parse(Path(__file__).with_name('api_client.py').read_text())
    node=next(item for item in tree.body if isinstance(item,ast.AsyncFunctionDef) and item.name=='request')
    values=dict(asyncio=asyncio,json=json,urllib=urllib,http=http,ssl=ssl,uuid=uuid,TOKEN='private-fixture-token',REPORT={})
    exec(compile(ast.Module(body=[node],type_ignores=[]),'api_client.py','exec'),values)
    return values


def connection(status=200,body=b'{"ok":true}'):
    client=MagicMock()
    response=MagicMock(status=status)
    response.read.return_value=body
    client.getresponse.return_value=response
    return client


class AcceptanceTransport(unittest.IsolatedAsyncioTestCase):
    async def test_get_recovers_without_exporting_reason_or_credentials(self):
        values=request_function(); failed=connection(); success=connection()
        failed.getresponse.side_effect=TimeoutError('PRIVATE-REASON-MUST-NOT-LEAK')
        with patch('http.client.HTTPSConnection',side_effect=[failed,success]) as factory, patch('asyncio.sleep',new_callable=AsyncMock):
            result=await values['request']('GET','/v1/calls/fixture')
        self.assertTrue(result['ok']);self.assertEqual(factory.call_count,2)
        self.assertEqual(values['REPORT']['transport_failures'][0]['stage'],'response')
        self.assertNotIn('PRIVATE-REASON',json.dumps(values['REPORT']))
        self.assertNotIn(values['TOKEN'],json.dumps(values['REPORT']))

    async def test_http_authorization_denial_is_never_retried(self):
        values=request_function(); denied=connection(403,b'{"error":{"code":"INSUFFICIENT_SCOPE"}}')
        with patch('http.client.HTTPSConnection',return_value=denied) as factory:
            with self.assertRaises(AssertionError):await values['request']('GET','/v1/calls/fixture')
        self.assertEqual(factory.call_count,1)
        self.assertEqual(values['REPORT']['failed_request']['code'],'INSUFFICIENT_SCOPE')

    async def test_ambiguous_mutation_is_never_replayed(self):
        values=request_function(); failed=connection()
        failed.getresponse.side_effect=TimeoutError('response lost after write')
        with patch('http.client.HTTPSConnection',return_value=failed) as factory:
            with self.assertRaises(TimeoutError):await values['request']('POST','/v1/calls',{'kind':'VOICE'})
        self.assertEqual(factory.call_count,1);failed.request.assert_called_once()

    async def test_read_recovery_is_bounded_on_persistent_outage(self):
        values=request_function(); failed=connection();failed.connect.side_effect=ConnectionRefusedError()
        with patch('http.client.HTTPSConnection',return_value=failed) as factory, patch('asyncio.sleep',new_callable=AsyncMock):
            with self.assertRaises(ConnectionRefusedError):await values['request']('GET','/v1/calls/fixture')
        self.assertEqual(factory.call_count,3);failed.request.assert_not_called()

    async def test_post_may_recover_tls_setup_only_when_no_request_was_sent(self):
        values=request_function(); failed=connection();failed.connect.side_effect=TimeoutError('TLS setup')
        denied=connection(403,b'{"error":{"code":"INSUFFICIENT_SCOPE"}}')
        with patch('http.client.HTTPSConnection',side_effect=[failed,denied]), patch('asyncio.sleep',new_callable=AsyncMock):
            result=await values['request']('POST','/v1/enterprise/extensions',{},expected=(403,))
        failed.request.assert_not_called();denied.request.assert_called_once()
        self.assertEqual(result['error']['code'],'INSUFFICIENT_SCOPE')

    async def test_post_never_retries_a_request_transmission_failure(self):
        values=request_function(); failed=connection();failed.request.side_effect=ConnectionResetError()
        with patch('http.client.HTTPSConnection',return_value=failed) as factory:
            with self.assertRaises(ConnectionResetError):await values['request']('POST','/v1/calls',{})
        self.assertEqual(factory.call_count,1)
