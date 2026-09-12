import io
import json
import unittest

from ratatui_backend import BackendView


class BackendTests(unittest.TestCase):
    def view(self, answer):
        output=io.StringIO()
        view=BackendView(io.StringIO(json.dumps(answer)+'\n'),output)
        view.log=io.StringIO()
        return view,output

    def test_password_uses_prompt_channel_and_never_state_or_log(self):
        password='fixture-secret-123'
        view,output=self.view({'id':1,'value':password})
        self.assertEqual(view.secret(None),password)
        events=[json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(events[-1]['kind'],'password')
        self.assertNotIn(password,output.getvalue())
        self.assertNotIn(password,view.log.getvalue())

    def test_menu_rejects_arbitrary_values_and_stale_answers(self):
        for answer in ({'id':1,'value':'wrong'}, {'id':2,'value':'wpa2'}):
            view,_=self.view(answer)
            with self.assertRaises(ValueError):
                view.choose('Wi-Fi security',[{'value':'wpa2','label':'WPA2'}])

    def test_cancel_and_disconnect_never_return_a_confirmation(self):
        view,_=self.view({'id':1,'cancel':True})
        with self.assertRaises(KeyboardInterrupt):view.ask('Continue?')
        view.incoming=io.StringIO()
        with self.assertRaises(EOFError):view.ask('Continue?')

    def test_verification_progress_is_measured_and_stage_labels_survive(self):
        view,output=self.view({})
        view.stage(4,'Verifying installed image')
        view.progress('Verify installed: userdata 4096/8192 bytes (50%)')
        event=json.loads(output.getvalue().splitlines()[-1])
        self.assertEqual(event['step'],4)
        self.assertEqual(event['measurement'][:2],[4096,8192])
        self.assertEqual(event['detail'],'Verify installed: userdata')


if __name__=='__main__':unittest.main()
