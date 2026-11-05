import ast
import asyncio
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch


def helper():
    tree=ast.parse(Path(__file__).with_name('api_client.py').read_text())
    node=next(n for n in tree.body if isinstance(n,ast.AsyncFunctionDef) and n.name=='transferred_media')
    values=dict(asyncio=asyncio,REPORT={'checks':{}},call_view=AsyncMock(return_value={'state':'CONNECTED'}),received_media=AsyncMock())
    exec(compile(ast.Module(body=[node],type_ignores=[]),'api_client.py','exec'),values)
    return values


class TransferAcceptance(unittest.IsolatedAsyncioTestCase):
    async def test_old_track_is_removed_before_new_peer_frames_are_certified(self):
        values=helper();departed=False;seen=[]
        async def leave(*args,**kwargs):
            nonlocal departed
            departed=True;return {'ok':True}
        departing=SimpleNamespace(identity='old',cmd=leave)
        caller=SimpleNamespace(identity='caller',cmd=AsyncMock(side_effect=[
            {'connected':True,'remote':['old','target']},{'connected':True,'remote':['target']}]))
        target=SimpleNamespace(identity='target',cmd=AsyncMock(return_value={'connected':True,'remote':['caller']}))
        async def frames(room):
            self.assertTrue(departed);self.assertEqual(caller.cmd.call_count,2);seen.append(room.identity)
        values['received_media']=frames
        values['call_view']=AsyncMock(side_effect=[{'state':'TRANSFERRING'},{'state':'CONNECTED'}])
        with patch('asyncio.sleep',new_callable=AsyncMock):await values['transferred_media']('fixture',caller,departing,target)
        self.assertEqual(seen,['target','caller'])

    async def test_departing_party_retaining_access_fails_acceptance(self):
        values=helper();room=SimpleNamespace(identity='old',cmd=AsyncMock(return_value={'ok':False}))
        with self.assertRaises(AssertionError):await values['transferred_media']('fixture',room,room,room)
        values['received_media'].assert_not_called()

    async def test_missing_new_peer_never_certifies_an_unrelated_stream(self):
        values=helper();departing=SimpleNamespace(identity='old',cmd=AsyncMock(return_value={'ok':True}))
        caller=SimpleNamespace(identity='caller',cmd=AsyncMock(return_value={'connected':True,'remote':['unrelated']}))
        target=SimpleNamespace(identity='target',cmd=AsyncMock(return_value={'connected':True,'remote':['caller']}))
        with patch('asyncio.sleep',new_callable=AsyncMock),self.assertRaises(AssertionError):
            await values['transferred_media']('fixture',caller,departing,target)
        values['received_media'].assert_not_called()
