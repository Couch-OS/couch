import copy
import hashlib
import io
import json
import unittest
from unittest.mock import patch

import couch_tui
from couch_install import InstallError
import release_discovery as discovery


def fixture(tag='v0.1.0-alpha.1'):
    metadata = {'schema': 1, 'model': discovery.MODEL, 'version': tag, 'installable': False, 'files': []}
    data = json.dumps(metadata).encode()
    name = f'couch-{tag}-{discovery.MODEL}.json'
    release = {'id': 1, 'tag_name': tag, 'draft': False, 'prerelease': '-alpha.' in tag,
               'assets': [{'id': 2, 'name': name, 'state': 'uploaded', 'size': len(data),
                           'digest': 'sha256:' + hashlib.sha256(data).hexdigest(),
                           'browser_download_url': discovery.DOWNLOAD + tag + '/' + name}]}
    return release, data


class DiscoveryTests(unittest.TestCase):
    def test_drafts_channels_exact_version_and_numeric_sort(self):
        alpha, _ = fixture()
        newer, _ = fixture('v0.1.0-alpha.10')
        stable, _ = fixture('v0.1.0')
        draft, _ = fixture('v0.2.0')
        draft['draft'] = True
        values = [alpha, draft, stable, newer]
        self.assertEqual([v.tag for v in discovery.selections(values)], ['v0.1.0'])
        self.assertEqual([v.tag for v in discovery.selections(values, 'alpha')], ['v0.1.0-alpha.10', 'v0.1.0-alpha.1'])
        self.assertEqual(len(discovery.selections(values, 'alpha', 'v0.1.0-alpha.1')), 1)
        self.assertEqual(discovery.selections(values, 'stable', 'v0.2.0'), [])

    def test_empty_and_source_only_releases_are_not_installation_choices(self):
        release, _ = fixture()
        release['assets'] = []
        self.assertEqual(discovery.selections([release], 'alpha'), [])
        self.assertEqual(discovery.discover(fetch=lambda *_: b'[]'), [])

    def test_listing_only_calls_bounded_releases_api_never_latest_or_assets(self):
        release, _ = fixture()
        calls = []
        def fetch(url, cap):
            calls.append((url, cap))
            return json.dumps([release]).encode()
        choices = discovery.discover('alpha', fetch=fetch)
        self.assertEqual(len(choices), 1)
        self.assertEqual(calls, [(discovery.API + '?per_page=100&page=1', discovery.MAX_LIST)])

    def test_bad_filters_fail_before_network(self):
        with patch.object(discovery, 'fetch_bytes') as fetch:
            with self.assertRaises(InstallError):
                discovery.discover('nightly', fetch=fetch)
            fetch.assert_not_called()

    def test_asset_pin_rejects_path_size_hash_and_duplicate_confusion(self):
        release, _ = fixture()
        for field, value in [('browser_download_url', 'https://example.com/evil'),
                             ('browser_download_url', release['assets'][0]['browser_download_url'] + '?different=1'),
                             ('size', True), ('size', discovery.MAX_MANIFEST + 1),
                             ('digest', None), ('state', 'new')]:
            bad = copy.deepcopy(release)
            bad['assets'][0][field] = value
            with self.assertRaises(InstallError): discovery.selections([bad], 'alpha')
        release['assets'] *= 2
        with self.assertRaises(InstallError): discovery.selections([release], 'alpha')

    def test_selection_is_frozen_and_changed_manifest_rejected(self):
        release, data = fixture()
        selected = discovery.selections([release], 'alpha')[0]
        self.assertFalse(selected.record()['installation_authorized'])
        with self.assertRaises(AttributeError): selected.tag = 'latest'
        with self.assertRaisesRegex(InstallError, 'changed'):
            discovery.inspect_manifest(selected, fetch=lambda *_: data + b' ')
        self.assertEqual(discovery.inspect_manifest(selected, fetch=lambda *_: data)['files'], [])

    def test_manifest_model_version_file_paths_and_empty_installable(self):
        for field, value in [('model', 'other'), ('version', 'v9.9.9'), ('installable', True),
                             ('files', [{'name': '../payload'}])]:
            release, data = fixture()
            metadata = json.loads(data)
            metadata[field] = value
            data = json.dumps(metadata).encode()
            release['assets'][0].update(size=len(data), digest='sha256:' + hashlib.sha256(data).hexdigest())
            selected = discovery.selections([release], 'alpha')[0]
            with self.assertRaises(InstallError): discovery.inspect_manifest(selected, fetch=lambda *_: data)

    def test_manifest_valid_versioned_file_metadata_still_not_authorization(self):
        release, data = fixture()
        metadata = json.loads(data)
        name = 'couch-installer-v0.1.0-alpha.1-linux-x86_64.tar.gz'
        metadata['files'] = [{'name': name, 'size': 1024, 'sha256': 'a'*64,
                              'url': discovery.DOWNLOAD + metadata['version'] + '/' + name}]
        metadata['installable'] = True
        data = json.dumps(metadata).encode()
        release['assets'][0].update(size=len(data), digest='sha256:' + hashlib.sha256(data).hexdigest())
        selected = discovery.selections([release], 'alpha')[0]
        self.assertTrue(discovery.inspect_manifest(selected, fetch=lambda *_: data)['installable'])
        self.assertFalse(selected.record()['installation_authorized'])

    def test_tui_explicit_metadata_browse_does_not_prepare_or_apply_install(self):
        release, data = fixture()
        choices = discovery.selections([release], 'alpha')
        output = io.StringIO()
        terminal = couch_tui.Terminal(io.StringIO('r\nalpha\n\n1\nq\n'), output)
        adapter = couch_tui.CoreAdapter(couch_tui.parser().parse_args([]))
        with patch.object(discovery, 'discover', return_value=choices), \
             patch.object(discovery, 'inspect_manifest', return_value=json.loads(data)):
            self.assertEqual(terminal.run(adapter), 0)
        self.assertIsNone(adapter.prepared)
        self.assertEqual(terminal.release_selection, choices[0])
        self.assertIn('Public flashing remains disabled', output.getvalue())
        self.assertIn('WiFi', output.getvalue())


if __name__ == '__main__':
    unittest.main()
