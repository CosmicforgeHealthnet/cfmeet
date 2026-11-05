import ast
import asyncio
from pathlib import Path
import unittest
from unittest.mock import AsyncMock, patch


def helpers(results):
    tree=ast.parse(Path(__file__).with_name('api_client.py').read_text())
    nodes=[n for n in tree.body if isinstance(n,(ast.FunctionDef,ast.AsyncFunctionDef)) and n.name in ('connect_failure','reconnect')]
    rooms=[]

    class Room:
        def __init__(self):self.disconnected=False;self.tokens=[]
        async def cmd(self,command,**body):
            self.tokens.append(body['token']);return results.pop(0)
        async def disconnect(self):self.disconnected=True

    class MediaClient:
        @staticmethod
        async def start():
            room=Room();rooms.append(room);return room

    values=dict(asyncio=asyncio,REPORT={'checks':{}},ROOMS=[],MediaClient=MediaClient)
    exec(compile(ast.Module(body=nodes,type_ignores=[]),'api_client.py','exec'),values)
    return values,rooms


class ReconnectAcceptance(unittest.IsolatedAsyncioTestCase):
    async def test_authorization_refusal_fails_without_a_second_attempt(self):
        values,rooms=helpers([{'ok':False,'error':'ConnectError: signal failure: 401 media connection not admitted'}])
        with patch('asyncio.sleep',new_callable=AsyncMock),self.assertRaises(AssertionError):
            await values['reconnect']('wss://media.example','current')
        self.assertEqual(len(rooms),1)
        self.assertEqual(values['REPORT']['media_reconnect_failures'],[{'error_class':'ConnectError','category':'REFUSED','attempt':1}])

    async def test_transport_fault_retries_once_with_the_same_current_credential(self):
        values,rooms=helpers([{'ok':False,'error':'TimeoutError: '},{'ok':True,'identity':'session-b'}])
        with patch('asyncio.sleep',new_callable=AsyncMock):
            room=await values['reconnect']('wss://media.example','current')
        self.assertEqual(room.identity,'session-b')
        self.assertEqual([r.tokens for r in rooms],[['current'],['current']])
        self.assertTrue(rooms[0].disconnected)
        self.assertEqual(values['REPORT']['media_reconnect_failures'][0]['category'],'TRANSPORT')

    async def test_two_transport_faults_still_fail_acceptance(self):
        values,rooms=helpers([{'ok':False,'error':'TimeoutError: '},{'ok':False,'error':'TimeoutError: '}])
        with patch('asyncio.sleep',new_callable=AsyncMock),self.assertRaises(AssertionError):
            await values['reconnect']('wss://media.example','current')
        self.assertEqual(len(values['REPORT']['media_reconnect_failures']),2)

    def test_failure_category_never_echoes_raw_sdk_text(self):
        values,_=helpers([])
        failure=values['connect_failure']('wss://media.example/rtc?access_token=secret: boom')
        self.assertEqual(failure,{'error_class':'RTC_ERROR','category':'TRANSPORT'})


if __name__=='__main__':
    unittest.main()
