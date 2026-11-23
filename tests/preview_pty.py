#!/usr/bin/env python3
"""Filesystem previews through Lig's real Zsh PTY and a private tmux screen."""

import base64
import fcntl
import json
import locale
import os
import re
import shlex
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import unittest
import zipfile
import zlib
from pathlib import Path
from unittest.mock import patch

from fish_pty import BIN, Fish

ZSH = os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh')
MARKS = [f'ROW_{n:02d}' for n in range(1, 31)]


def png(width=4, height=4, pixels=None):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    if pixels is None:
        pixels = b'\xff\x00\x99' * (width * height)
    stride = width * 3
    rows = b''.join(b'\0' + pixels[i:i + stride] for i in range(0, len(pixels), stride))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def wait_pattern(term, pattern, start):
    term.wait_for(lambda: re.search(pattern, term.output[start:]))
    return re.search(pattern, term.output[start:])


@unittest.skipUnless(ZSH, 'zsh is required')
class PreviewPTY(unittest.TestCase):
    def shell(self, config='', after_config='', terminal=''):
        with patch.dict(os.environ, {'TERM_PROGRAM': terminal, 'COLORTERM': 'truecolor'}):
            for name in ('TMUX', 'TMUX_PANE', 'STY', 'ZELLIJ'):
                os.environ.pop(name, None)
            term = Fish(wrapped=True, shell=ZSH, after_config='cd "$HOME"\n' + after_config, lig_config=config)
        self.addCleanup(lambda: self.assertEqual(term.close(17), 17))
        return term

    def test_disabled_preserves_popup_and_does_not_execute_backend(self):
        term = self.shell(
            """[preview]
enabled = false
[preview.text]
backend = "shell"
script = "touch unexpected; printf EXTERNAL"
"""
        )
        (term.home / 'sample-a.txt').write_text('NATIVE_CONTENT')
        (term.home / 'sample-b.txt').write_text('NATIVE_CONTENT')
        start = len(term.output)
        term.send('cat sample-')
        term.wait_popup('sample-a.txt', start)
        term.send(b'\t\r')
        self.assertEqual(term.buffer(), 'cat sample-b.txt')
        self.assertNotIn(b'EXTERNAL', term.plain(start))
        self.assertNotIn(b'\x1b[?1006h', term.output[start:])
        self.assertFalse((term.home / 'unexpected').exists())

    def test_inline_scroll_selection_toggle_and_mouse_restore(self):
        term = self.shell(
            """[preview]
enabled = true
layout = "inline"
position = "right"
width = 30
height = 8
"""
        )
        (term.home / 'sample-a.txt').write_text('\n'.join(MARKS))
        (term.home / 'sample-b.txt').write_text('OTHER_FILE')
        start = len(term.output)
        term.send('cat sample-')
        term.wait_text('ROW_01', start)
        self.assertIn(b' 1 ROW_01', term.plain(start))
        header = list(re.finditer(rb'\x1b\[(\d+);(\d+)H(?:(?!\x1b\[\d+;\d+H).){0,160}'
                                  + '╭ sample-a.txt '.encode(), term.output[start:]))
        self.assertTrue(header, term.output[start:])
        row, col = (int(v) for v in header[-1].groups())
        start = len(term.output)
        term.send(f'\x1b[<65;{col + 1};{row + 2}M')
        term.wait_text('ROW_09', start)
        self.assertIn(b' 4 ROW_04', term.plain(start))
        start = len(term.output)
        term.send(b'\x1b[6~')
        term.wait_text('ROW_14', start)
        start = len(term.output)
        term.send(b'\t')
        term.wait_text('OTHER_FILE', start)
        start = len(term.output)
        term.send(b'\x1bp')
        term.wait_for(lambda: b'\x1b[?1006l' in term.output[start:])
        self.assertNotIn(b'OTHER_FILE', term.plain(start))
        term.send(b'\r')
        self.assertEqual(term.buffer(), 'cat sample-b.txt')

    def test_native_image_then_archive_updates_the_same_popup(self):
        term = self.shell('[preview]\nenabled = true\nlayout = "inline"\n')
        (term.home / 'preview-a.png').write_bytes(png())
        with zipfile.ZipFile(term.home / 'preview-b.zip', 'w', zipfile.ZIP_DEFLATED) as archive:
            archive.writestr('nested/item.txt', b'four')
        before = len(term.output)
        term.send('cat preview-')
        wait_pattern(term, rb';48;2;255;0;153m', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('nested/item.txt', before)
        self.assertNotIn(b';48;2;255;0;153m', term.output[before:])
        self.assertFalse((term.home / 'nested').exists())

    def test_native_archive_and_webp_without_tools(self):
        term = self.shell('[preview]\nenabled = true\nlayout = "inline"\nimage_protocol = "cells"\n',
                          "export PATH=''\n")
        fixtures = Path(__file__).parent / 'fixtures'
        with zipfile.ZipFile(term.home / 'local-a.zip', 'w') as archive:
            archive.writestr('nested/item.txt', b'four')
        # A 10×10 lossless WebP filled with #ff0099.
        (term.home / 'local-b.webp').write_bytes(base64.b64decode('UklGRhwAAABXRUJQVlA4TBAAAAAvCUACAAcQ/W9m/wMR0f8A'))
        shutil.copyfile(fixtures / 'media' / 'clip.webm', term.home / 'local-c.webm')
        shutil.copyfile(fixtures / 'archives' / 'sample.7z', term.home / 'local-d.data')
        before = len(term.output)
        term.send('cat local-')
        term.wait_text('nested/item.txt', before)
        before = len(term.output)
        term.send(b'\t')
        wait_pattern(term, rb';48;2;255;0;153m', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('Modified:', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('Modified:', before)

    def test_configured_7zip_lists_unicode_and_falls_back_for_encrypted_headers(self):
        binary = shutil.which('7zz') or shutil.which('7z')
        if not binary:
            self.skipTest('7-Zip CLI is required')
        command = json.dumps([binary, 'l', '-slt', '-sccUTF-8', '-bd', '-spd', '--', '{file}'])
        term = self.shell(
            f"""[preview]
enabled = true
layout = "inline"
width = "60%"
[preview.archive]
backend = "command"
command = {command}
output = "seven_zip_list"
"""
        )
        fixtures = Path(__file__).parent / 'fixtures' / 'archives'
        shutil.copyfile(fixtures / 'sample.7z', term.home / 'archive-a .data')
        shutil.copyfile(fixtures / 'hidden.7z', term.home / 'archive-b.7z')
        before = len(term.output)
        term.send('cat archive-')
        term.wait_text('nested/漢字-', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('Modified:', before)
        self.assertNotIn(b'Enter password', term.plain(before))
        self.assertFalse((term.home / 'nested').exists())

    def test_configured_mediainfo_shows_tracks_and_follows_selection(self):
        binary = shutil.which('mediainfo')
        if not binary:
            self.skipTest('MediaInfo CLI is required')
        command = json.dumps([
            binary, '--Output=JSON', '--File_CheckSideCarFiles=0', '--File_TestContinuousFileNames=0', '--', '{file}',
        ])
        term = self.shell(
            f"""[preview]
enabled = true
layout = "inline"
width = "60%"
[preview.media]
backend = "command"
command = {command}
output = "mediainfo_json"
"""
        )
        fixtures = Path(__file__).parent / 'fixtures' / 'media'
        shutil.copyfile(fixtures / 'tracks.mkv', term.home / 'media-a 漢.data')
        (term.home / 'media-b.m4a').write_bytes(b'invalid')
        (term.home / 'media-c.ts').write_text('const followSelection = true')
        shutil.copyfile(fixtures / 'clip.webm', term.home / 'media-d.data')
        before = len(term.output)
        term.send('cat media-')
        term.wait_text('Subtitles 2', before)
        self.assertIn(b'Audio 2', term.plain(before))
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('Modified:', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('followSelection', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('VP9', before)
        self.assertIn(b'WebM', term.plain(before))

    def test_media_category_and_extension_overrides(self):
        term = self.shell(
            """[preview]
enabled = true
layout = "inline"
[preview.media]
backend = "command"
command = ["printf", "MEDIA_CATEGORY"]
[preview.rules.mp4]
backend = "command"
command = ["printf", "MEDIA_EXTENSION"]
"""
        )
        fixtures = Path(__file__).parent / 'fixtures' / 'media'
        shutil.copyfile(fixtures / 'tone.flac', term.home / 'media-a.flac')
        shutil.copyfile(fixtures / 'tail.mp4', term.home / 'media-b.mp4')
        before = len(term.output)
        term.send('cat media-')
        term.wait_text('MEDIA_CATEGORY', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('MEDIA_EXTENSION', before)

    def test_missing_media_command_uses_file_metadata(self):
        term = self.shell(
            """[preview]
enabled = true
layout = "inline"
[preview.media]
backend = "command"
command = ["/no/such/lig-test-mediainfo", "{file}"]
output = "mediainfo_json"
"""
        )
        fixtures = Path(__file__).parent / 'fixtures' / 'media'
        shutil.copyfile(fixtures / 'clip.webm', term.home / 'media-a.webm')
        shutil.copyfile(fixtures / 'tail.mp4', term.home / 'media-b.mp4')
        before = len(term.output)
        term.send('cat media-')
        term.wait_text('Modified:', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('Modified:', before)

    def test_configured_command_and_shell_use_real_viewport(self):
        term = self.shell(
            """[preview]
enabled = true
layout = "inline"
position = "right"
width = 32
height = 8
[preview.text]
backend = "command"
command = ["printf", "%s %s", "ARGS", "{width}x{height}"]
[preview.rules.log]
backend = "shell"
script = 'printf "SHELL:%s:%s" "$LIG_PREVIEW_EXT" "$LIG_PREVIEW_HEIGHT"'
"""
        )
        (term.home / 'sample-a.txt').touch()
        (term.home / 'sample-b.log').touch()
        before = len(term.output)
        term.send('cat sample-')
        term.wait_text('ARGS 30x6', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('SHELL:log:6', before)

    def test_hidden_preview_and_live_file_refresh_without_help_labels(self):
        term = self.shell(
            """[preview]
enabled = true
start_hidden = true
show_title = false
show_controls = true
[preview.keys]
close = "f7"
"""
        )
        selected = term.home / 'sample-a.txt'
        selected.write_text('INITIAL_SELECTED')
        (term.home / 'sample-b.txt').touch()
        before = len(term.output)
        term.send('cat sample-')
        term.wait_popup('sample-a.txt', before)
        self.assertNotIn(b'alt-p preview', term.plain(before))
        self.assertNotIn(b'INITIAL_SELECTED', term.plain(before))
        before = len(term.output)
        term.send(b'\x1bp')
        term.wait_text('INITIAL_SELECTED', before)
        self.assertNotIn(b'f7 hide', term.plain(before))
        self.assertNotIn('╭ sample-a.txt '.encode(), term.plain(before))
        before = len(term.output)
        selected.write_text('CHANGED_SELECTED_FILE')
        term.wait_text('CHANGED_SELECTED_FILE', before)
        before = len(term.output)
        term.send(b'\x1b[18~')
        term.wait_for(lambda: b'\x1b[?1006l' in term.output[before:])
        self.assertNotIn(b'alt-p preview', term.plain(before))
        self.assertNotIn(b'CHANGED_SELECTED_FILE', term.plain(before))

    def test_tree_reuses_completion_icon_overrides_and_refreshes_nested_changes(self):
        term = self.shell(
            """[ui]
icons = "unicode"
[icons]
"*.rs" = { glyph = "R", color = "202" }
folder = { glyph = "D", color = "208" }
[preview]
enabled = true
layout = "inline"
"""
        )
        deep = term.home / 'catalog-a/sub/child'
        deep.mkdir(parents=True)
        (term.home / 'catalog-b').mkdir()
        (deep / 'tree.rs').touch()
        (deep / 'below-limit').mkdir()
        (deep / 'below-limit/EXCLUDED').touch()
        before = len(term.output)
        term.send('cd catalog-')
        term.wait_text('R tree.rs', before)
        self.assertIn(b'\x1b[38;5;202mR ', term.output[before:])
        self.assertIn(b'\x1b[38;5;208mD ', term.output[before:])
        self.assertNotIn(b'EXCLUDED', term.plain(before))
        before = len(term.output)
        (deep / 'new.rs').touch()
        term.wait_text('R new.rs', before)

    def test_progressive_jpeg_renders_and_utf16_file_is_text(self):
        # 12×8 solid color, encoded progressively by ImageMagick.
        jpeg = base64.b64decode(
            '/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgICAgMCAgIDAwMDBAYEBAQEBAgGBgUGCQgKCgkICQkKDA8MCgsOCwkJ'
            'DRENDg8QEBEQCgwSExIQEw8QEBD/2wBDAQMDAwQDBAgEBAgQCwkLEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ'
            'EBAQEBAQEBAQEBAQEBAQEBAQEBD/wgARCAAIAAwDAREAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAb/xAAWAQEBAQ'
            'AAAAAAAAAAAAAAAAAABgj/2gAMAwEAAhADEAAAAYSX0CP/xAAUEAEAAAAAAAAAAAAAAAAAAAAQ/9oACAEBAAEFAj//'
            'xAAUEQEAAAAAAAAAAAAAAAAAAAAQ/9oACAEDAQE/AT//xAAUEQEAAAAAAAAAAAAAAAAAAAAQ/9oACAECAQE/AT//xAAUEAE'
            'AAAAAAAAAAAAAAAAAAAAQ/9oACAEBAAY/Aj//xAAUEAEAAAAAAAAAAAAAAAAAAAAQ/9oACAEBAAE/IT//2gAMAwEAAgAD'
            'AAAAELf/xAAUEQEAAAAAAAAAAAAAAAAAAAAQ/9oACAEDAQE/ED//xAAUEQEAAAAAAAAAAAAAAAAAAAAQ/9oACAECAQE/ED//'
            'xAAUEAEAAAAAAAAAAAAAAAAAAAAQ/9oACAEBAAE/ED//2Q=='
        )
        self.assertIn(b'\xff\xc2', jpeg)
        term = self.shell('[preview]\nenabled=true\nlayout="inline"\n')
        (term.home / 'sample-a.JPEG').write_bytes(jpeg)
        (term.home / 'sample-b.txt').write_bytes(b'\xff\xfe' + 'UTF16_CONTENT'.encode('utf-16-le'))
        before = len(term.output)
        term.send('cat sample-')
        wait_pattern(term, rb';48;2;2\d\d;\d{1,2};\d{1,2}m', before)
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('UTF16_CONTENT', before)

    def test_image_cells_can_be_forced_without_terminal_queries(self):
        config = '[preview]\nenabled=true\nlayout="inline"\nimage_protocol='
        for mode in ('cells', 'half_blocks'):
            with self.subTest(mode=mode):
                term = self.shell(config + f'"{mode}"\n', terminal='iTerm.app')
                (term.home / 'sample-a.png').write_bytes(png())
                (term.home / 'sample-b.txt').touch()
                before = len(term.output)
                term.send('cat sample-')
                wait_pattern(term, rb';48;2;255;0;153m', before)
                self.assertNotIn(b'\x1b]1337;ReportCellSize', term.output[before:])
                self.assertNotIn(b'\x1b_G', term.output[before:])
                if mode == 'half_blocks':
                    self.assertIn('▀'.encode(), term.output[before:])

    def test_kitty_probe_chunks_and_owned_cleanup(self):
        term = self.shell('[preview]\nenabled=true\nlayout="inline"\n')
        (term.home / 'sample-a.png').write_bytes(png(48, 36, os.urandom(48 * 36 * 3)))
        (term.home / 'sample-b.txt').write_text('AFTER_GRAPHIC')
        before = len(term.output)
        term.send('cat sample-')
        probe = wait_pattern(term, rb'\x1b_Gi=(\d+),s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\', before)[1]
        os.write(term.fd, b'\x1b_Gi=')
        term.read(.004)
        os.write(term.fd, probe + b';OK\x1b\\')
        first = wait_pattern(term, rb'\x1b_Ga=T,t=d,f=100,i=(\d+),c=\d+,r=\d+,C=1,m=1;([A-Za-z0-9+/=]+)\x1b\\', before)
        final_chunk = wait_pattern(term, rb'\x1b_Gm=0;([A-Za-z0-9+/=]+)\x1b\\', before + first.end())
        upload = term.output[before + first.end():before + first.end() + final_chunk.end()]
        chunks = [first[2], *re.findall(rb'\x1b_Gm=\d;([A-Za-z0-9+/=]+)\x1b\\', upload)]
        self.assertTrue(all(len(chunk) <= 4096 for chunk in chunks))
        self.assertTrue(base64.b64decode(b''.join(chunks)).startswith(b'\x89PNG\r\n\x1a\n'))
        os.write(term.fd, b'\x1b_Gi=' + first[1] + b';OK\x1b\\')
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('AFTER_GRAPHIC', before)
        self.assertIn(b'\x1b_Ga=d,d=I,i=' + first[1] + b',q=2\x1b\\', term.output[before:])
        self.assertEqual(term.buffer(), 'cat sample-')

    def test_kitty_upload_error_restores_cell_rendering(self):
        term = self.shell('[preview]\nenabled=true\nlayout="inline"\n')
        (term.home / 'sample-a.png').write_bytes(png())
        (term.home / 'sample-b.txt').touch()
        before = len(term.output)
        term.send('cat sample-')
        probe = wait_pattern(term, rb'\x1b_Gi=(\d+),s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\', before)[1]
        os.write(term.fd, b'\x1b_Gi=' + probe + b';OK\x1b\\')
        image_id = wait_pattern(term, rb'\x1b_Ga=T,t=d,f=100,i=(\d+)', before)[1]
        before = len(term.output)
        os.write(term.fd, b'\x1b_Gi=' + image_id + b';EINVAL\x1b\\')
        wait_pattern(term, rb';48;2;255;0;153m', before)
        self.assertIn(b'\x1b_Ga=d,d=I,i=' + image_id, term.output[before:])
        self.assertEqual(term.buffer(), 'cat sample-')

    def test_iterm_replaces_same_size_image_and_clears_before_text(self):
        term = self.shell('[preview]\nenabled=true\nlayout="inline"\n', terminal='iTerm.app')
        for name in ('sample-a.png', 'sample-b.png'):
            (term.home / name).write_bytes(png())
        (term.home / 'sample-c.txt').write_text('AFTER_INLINE_IMAGE')
        before = len(term.output)
        term.send('cat sample-')
        wait_pattern(term, rb'\x1b\]1337;ReportCellSize\x1b\\', before)
        os.write(term.fd, b'\x1b]1337;ReportCellSize=17.50;8.00;2.0\x07')
        sequence = rb'\x1b\]1337;File=inline=1;width=\d+;height=\d+;preserveAspectRatio=1:([A-Za-z0-9+/=]+)\x07'
        match = wait_pattern(term, sequence, before)
        self.assertTrue(base64.b64decode(match[1]).startswith(b'\x89PNG'))
        term.QUERIES = {**term.QUERIES, b'\x1b[6n': b'\x1b[1;18R'}
        before = len(term.output)
        fcntl.ioctl(term.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 1000, 600))
        resized = wait_pattern(term, sequence, before)
        self.assertRegex(term.output[before:before + resized.start()], rb'\x1b\[\d+X')
        before = len(term.output)
        term.send(b'\t')
        next_image = wait_pattern(term, sequence, before)
        self.assertRegex(term.output[before:before + next_image.start()], rb'\x1b\[\d+X')
        before = len(term.output)
        term.send(b'\t')
        term.wait_text('AFTER_INLINE_IMAGE', before)
        self.assertRegex(term.output[before:], rb'\x1b\[\d+X')
        self.assertNotRegex(term.output[before:], sequence)

    def test_floating_portrait_fits_the_frame_then_text_recovers_its_width(self):
        for mode in ('cells', 'half_blocks', 'kitty', 'iterm'):
            with self.subTest(mode=mode):
                protocol = 'auto' if mode in ('kitty', 'iterm') else mode
                term = self.shell(f"""[preview]
enabled = true
layout = "floating"
width = 40
height = 14
adaptive_size = false
image_protocol = "{protocol}"
""", terminal='iTerm.app' if mode == 'iterm' else '')
                (term.home / 'sample-a.png').write_bytes(png(24, 44))
                (term.home / 'sample-b.txt').write_text('AFTER_PORTRAIT')
                before = len(term.output)
                term.send('cat sample-')
                header = rb'\x1b\[11;65H\x1b\[[0-9;]+m' + '╭ sample-a.pn ─╮'.encode()
                wait_pattern(term, header, before)
                if mode == 'kitty':
                    probe = wait_pattern(term, rb'\x1b_Gi=(\d+),s=1,v=1,a=q,t=d,f=24;AAAA', before)[1]
                    os.write(term.fd, b'\x1b_Gi=' + probe + b';OK\x1b\\')
                    upload = wait_pattern(term, rb'\x1b\[12;66H\x1b_Ga=T,t=d,f=100,i=(\d+),c=14,r=12', before)
                    os.write(term.fd, b'\x1b_Gi=' + upload[1] + b';OK\x1b\\')
                elif mode == 'iterm':
                    wait_pattern(term, rb'\x1b\]1337;ReportCellSize\x1b\\', before)
                    os.write(term.fd, b'\x1b]1337;ReportCellSize=16;8\x07')
                    wait_pattern(term, rb'\x1b\[12;66H\x1b\]1337;File=inline=1;width=14;height=12;', before)
                else:
                    glyph = '▀' if mode == 'half_blocks' else ' '
                    tail = rb'\x1b\[0;48;5;235m\x1b\[[0-9;]+m' + '│'.encode()
                    self.assertRegex(term.output[before:], glyph.encode() * 13)
                    self.assertRegex(term.output[before:], tail)
                before = len(term.output)
                term.send(b'\t')
                term.wait_text('AFTER_PORTRAIT', before)
                wait_pattern(term, rb'\x1b\[11;41H\x1b\[[0-9;]+m' + '╭ sample-b.txt '.encode(), before)

    def test_binary_fallback_formats_mtime_using_shell_locale(self):
        timestamp = 1635940800
        previous = locale.setlocale(locale.LC_TIME)
        try:
            locale.setlocale(locale.LC_TIME, 'C')
            default_date = time.strftime('%x', time.localtime(timestamp))
            try:
                locale.setlocale(locale.LC_TIME, 'it_IT.UTF-8')
            except locale.Error:
                self.skipTest('it_IT.UTF-8 locale is required')
            italian_date = time.strftime('%x', time.localtime(timestamp))
        finally:
            locale.setlocale(locale.LC_TIME, previous)
        self.assertNotEqual(italian_date, default_date)
        term = self.shell(
            '[preview]\nenabled=true\nlayout="inline"\n',
            'unset LC_ALL\nexport LANG=C LC_TIME=it_IT.UTF-8\n',
        )
        for name in ('sample-a.bin', 'sample-b.bin'):
            path = term.home / name
            path.write_bytes(b'\x00\x01\x02')
            os.utime(path, (timestamp, timestamp))
        before = len(term.output)
        term.send('cat sample-')
        term.wait_text(f'Modified: {italian_date}', before)

    def test_sigterm_restores_mouse_and_cancels_the_backend(self):
        term = Fish(wrapped=True, shell=ZSH, after_config='cd "$HOME"\n', lig_config="""[preview]
enabled = true
[preview.text]
backend = "shell"
script = 'touch started; (sleep 1; touch leaked) & wait'
""")
        status = None
        try:
            (term.home / 'sample-a.txt').touch()
            (term.home / 'sample-b.txt').touch()
            term.send('cat sample-')
            term.wait_for(lambda: (term.home / 'started').exists() and b'\x1b[?1006h' in term.output)
            before = len(term.output)
            os.kill(term.pid, signal.SIGTERM)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                term.read(.02)
                pid, result = os.waitpid(term.pid, os.WNOHANG)
                if pid:
                    status = os.waitstatus_to_exitcode(result)
                    break
            self.assertEqual(status, 128 + signal.SIGTERM)
            self.assertIn(b'\x1b[?1006l', term.output[before:])
            time.sleep(1)
            self.assertFalse((term.home / 'leaked').exists())
        finally:
            if status is None:
                os.kill(term.pid, signal.SIGKILL)
                os.waitpid(term.pid, 0)
            os.close(term.fd)
            term.tmp.cleanup()


@unittest.skipUnless(ZSH and shutil.which('tmux'), 'zsh and tmux are required')
class PreviewScreen(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='lig-preview-', dir='/tmp')
        self.addCleanup(self.cleanup_home, temp)
        self.home = Path(temp.name)
        self.base = ['tmux', '-S', str(self.home / 'socket'), '-f', '/dev/null']
        self.addCleanup(self.close_server)
        config = self.home / 'config/lig'
        config.mkdir(parents=True)
        runtime = self.home / 'runtime'
        runtime.mkdir(mode=0o700)
        (config / 'config.toml').write_text(
            '[preview]\nenabled = true\nlayout = "floating"\nposition = "auto"\nwidth = "40%"\nheight = "60%"\n'
        )
        (self.home / '.zshenv').write_text('skip_global_compinit=1\n')
        (self.home / '.zshrc').write_text(
            "PROMPT=$'FIRST_PROMPT_LINE\npreview-test> '\nRPROMPT='RIGHT_PROMPT'\n"
        )
        (self.home / 'pick-a.txt').write_text('MUST_RESTORE_A\n')
        (self.home / 'pick-b.txt').write_text('MUST_RESTORE_B\n')
        env = dict(os.environ, HOME=str(self.home), ZDOTDIR=str(self.home),
                   XDG_CONFIG_HOME=str(config.parent), XDG_RUNTIME_DIR=str(runtime),
                   XDG_STATE_HOME=str(self.home / 'state'), XDG_DATA_HOME=str(self.home / 'data'),
                   SHELL=ZSH, LIG_LOG='1')
        for key in ['LIG_TTY', 'LIG_DISABLE', 'LIG_PID', 'INSIDE_EMACS']:
            env.pop(key, None)
        self.env = env
        subprocess.run([BIN, 'install', 'zsh'], env=env, check=True, capture_output=True)

    def close_server(self):
        # Shut down Lig and its child PTY before killing the private tmux server.
        pane = subprocess.run(self.base + ['display-message', '-p', '-t', 'test:0.0', '#{pane_pid}'],
                              capture_output=True, text=True, timeout=5)
        pid = int(pane.stdout) if pane.returncode == 0 and pane.stdout.strip().isdecimal() else 0
        if pid > 0:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            for _ in range(20):
                if subprocess.run(self.base + ['has-session', '-t', 'test'], capture_output=True, timeout=5).returncode:
                    return
                time.sleep(.03)
        subprocess.run(self.base + ['kill-server'], capture_output=True, timeout=5)

    def cleanup_home(self, temp):
        # Zsh can be writing history as tmux's final hangup arrives.
        for _ in range(20):
            try:
                return temp.cleanup()
            except OSError:
                time.sleep(.05)
        shutil.rmtree(self.home, ignore_errors=True)

    def tmux(self, *args):
        return subprocess.run(self.base + list(args), env=self.env, capture_output=True, text=True,
                              timeout=5, check=True).stdout

    def screen(self):
        return self.tmux('capture-pane', '-pt', 'test:0.0')

    def wait(self, check):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            screen = self.screen()
            if check(screen):
                return screen
            time.sleep(.03)
        logs = '\n'.join(log.read_text(errors='replace')[-6000:] for log in self.home.rglob('lig.log'))
        self.fail(f'Condition missing from terminal screen:\n{screen}\n{logs}')

    @staticmethod
    def frame(screen, title='pick-a.txt', contains=None):
        lines = screen.splitlines()
        pattern = re.compile(r'╭[^╮]*╮' if contains else
                             r'╭─+╮' if title is None else r'╭ ' + re.escape(title) + r' [─]*╮')
        for row, line in enumerate(lines):
            for header in pattern.finditer(line):
                col, right = header.span()
                for bottom in range(row + 1, len(lines)):
                    if lines[bottom][col:col + 1] == '╰' and lines[bottom][right - 1:right] == '╯':
                        if not contains or any(contains in body[col:right] for body in lines[row + 1:bottom]):
                            return row, col, bottom - row + 1, right - col
                        break
        return None

    def start_popup(self, config, cols=100, rows=30, command='cat pick-'):
        (self.home / 'config/lig/config.toml').write_text(config)
        self.tmux('new-session', '-d', '-x', str(cols), '-y', str(rows), '-s', 'test', '-c', str(self.home),
                  ZSH, '-i')
        self.wait(lambda s: 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', command)

    def test_compact_frames_fit_native_text_and_ansi_output_in_both_layouts(self):
        (self.home / 'pick-a.txt').write_text('x    \n\n   \n')
        (self.home / 'pick-b.txt').unlink()
        (self.home / 'pick-b.log').touch()
        self.start_popup(r"""[ui]
icons = "none"
width = 70
[preview]
enabled = true
layout = "floating"
width = 32
height = 10
[preview.rules.log]
backend = "shell"
output = "ansi"
script = 'printf "%sx%s\n" "$LIG_PREVIEW_WIDTH" "$LIG_PREVIEW_HEIGHT" >> "$HOME/preview-viewport"; if [ "$LIG_PREVIEW_HEIGHT" -ge 25 ]; then printf "Unexpected large viewport\n"; else printf "S\033[41m   \033[0m    \n\n"; fi'
""")
        text = lambda s: self.frame(s, contains='1 x')
        ansi = lambda s: self.frame(s, contains='│S')
        compact = self.wait(lambda s: text(s) == (27, 95, 3, 5))
        self.assertIn('RIGHT_PROMPT', compact)
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        screen = self.wait(lambda s: ansi(s) == (27, 94, 3, 6))
        self.assertNotIn('1 x', screen)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        log = self.home / 'preview-viewport'
        screen = self.wait(lambda s: ansi(s) == (3, 94, 27, 6) and len(log.read_text().splitlines()) >= 2)
        self.assertNotIn('Unexpected', screen)
        self.assertEqual(log.read_text().splitlines(), ['30x8', '30x21'])
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        self.wait(lambda s: ansi(s) == (27, 94, 3, 6))
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        self.wait(lambda s: (pane := ansi(s)) and pane[0] < 27 and pane[2:] == (3, 6))
        self.tmux('send-keys', '-t', 'test:0.0', 'C-p')
        self.wait(lambda s: (pane := text(s)) and pane[0] < 27 and pane[2:] == (3, 5))

    def test_git_side_by_side_is_only_used_for_wide_expanded_floating_previews(self):
        git = shutil.which('git')
        if git is None:
            self.skipTest('git is required')
        tools = self.home / 'tools'
        tools.mkdir()
        (tools / 'git').symlink_to(git)
        delta = tools / 'delta'
        delta_path = shutil.which('delta')
        renderer = (f'exec {shlex.quote(delta_path)} --no-gitconfig "$@"' if delta_path
                    else r'''while IFS= read -r line; do :; done
printf '\033[41m%*s\033[0m\n' "${3#--width=}" ''
printf '%-54s │ %-54s\n' 'OLD_MARKER' 'NEW_MARKER' ''')
        delta.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> "$HOME/delta-args"\n' + renderer + '\n')
        delta.chmod(0o755)
        with (self.home / '.zshrc').open('a') as rc:
            rc.write('export PATH="$HOME/tools"\n')
        self.env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
        (self.home / 'pick-a.txt').write_text('same_line_value OLD_MARKER\n')
        for argv in [('init', '-q', '--template='), ('add', 'pick-a.txt', 'pick-b.txt')]:
            subprocess.run([git, *argv], cwd=self.home, env=self.env, check=True, capture_output=True)
        (self.home / 'pick-a.txt').write_text('same_line_value NEW_MARKER\n')
        (self.home / 'pick-b.txt').write_text('OTHER_CHANGE\n')
        self.start_popup("""[ui]
width = 48
[preview]
enabled = true
layout = "floating"
width = 32
height = 8
""", cols=120, command='git diff pick-')
        unified = lambda s: self.frame(s, contains='+same_line_value NEW_MARKER')
        side_by_side = lambda s: any('OLD_MARKER' in row and 'NEW_MARKER' in row for row in s.splitlines())
        args = self.home / 'delta-args'
        compact = unified(self.wait(unified))
        self.assertFalse(args.exists())
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        expanded = self.wait(side_by_side)
        self.assertGreaterEqual(self.frame(expanded)[3], 82)
        self.assertGreater(self.frame(expanded)[2], compact[2])
        self.assertIn('RIGHT_PROMPT', expanded)
        self.assertEqual(args.read_text().splitlines()[:3],
                         ['--side-by-side', '--paging=never', '--width=118'])

        self.tmux('resize-window', '-t', 'test:0', '-x', '76', '-y', '30')
        narrow = unified(self.wait(lambda s: unified(s) and not side_by_side(s)))
        self.assertEqual(narrow[3], compact[3])
        self.assertGreater(narrow[2], compact[2])
        self.tmux('resize-window', '-t', 'test:0', '-x', '120', '-y', '30')
        self.wait(side_by_side)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        self.wait(lambda s: unified(s) and unified(s)[0] < compact[0] and unified(s)[2:] == compact[2:])
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        self.wait(side_by_side)
        delta.unlink()
        self.wait(lambda s: (pane := unified(s)) and pane[3] == compact[3] and pane[2] >= narrow[2])
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        self.wait(lambda s: unified(s) == compact)

    def test_images_fit_both_layouts_and_keep_separate_floating_presets(self):
        (self.home / 'pick-a.txt').unlink()
        (self.home / 'pick-b.txt').unlink()
        (self.home / 'pick-a.png').write_bytes(png(120, 16))
        (self.home / 'pick-b.png').write_bytes(png(24, 44))
        (self.home / 'pick-c.txt').write_text('MUST_RESTORE_C\n')
        self.start_popup("""[ui]
width = 40
[preview]
enabled = true
layout = "inline"
image_protocol = "cells"
width = 40
height = 14
""", rows=36)

        def framed(title, check):
            return self.frame(self.wait(lambda s: (pane := self.frame(s, title)) and check(pane)), title)

        landscape, portrait, text = 'pick-a.png', 'pick-b.png', 'pick-c.txt'
        framed(landscape, lambda p: p[0] == 2 and p[2:] == (5, 40))
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        framed(portrait, lambda p: p[0] == 2 and p[2:] == (14, 16))
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        compact_image = framed(portrait, lambda p: p == (22, 84, 14, 16))
        self.tmux('send-keys', '-t', 'test:0.0', 'C-p')
        framed(landscape, lambda p: p == (31, 60, 5, 40))
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab', 'Tab')
        compact_text = framed(text, lambda p: p == (33, 82, 3, 18))
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        tall_text = framed(text, lambda p: p[2] > 20 and p[3] == 18)
        self.tmux('send-keys', '-t', 'test:0.0', 'C-p')
        framed(portrait, lambda p: p == compact_image)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        middle = framed(portrait, lambda p: p[2] > compact_image[2] and p[3] > compact_image[3])
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        large = framed(portrait, lambda p: p[2] > middle[2] and p[3] > middle[3])
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        framed(text, lambda p: p == tall_text)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        framed(text, lambda p: p == compact_text)
        self.tmux('send-keys', '-t', 'test:0.0', 'C-p')
        framed(portrait, lambda p: p == large)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        framed(portrait, lambda p: p == compact_image)

    def test_directory_archive_and_metadata_fit_native_rows(self):
        (self.home / 'pick-a.txt').unlink()
        (self.home / 'pick-b.txt').unlink()
        tree = self.home / 'pick-a'
        tree.mkdir()
        (tree / 'tree-file').touch()
        with zipfile.ZipFile(self.home / 'pick-b.zip', 'w') as archive:
            archive.writestr('small.txt', b'short')
        (self.home / 'pick-c.data').write_bytes(b'\0' * 16)
        self.start_popup("""[ui]
width = 40
icons = "none"
[preview]
enabled = true
layout = "inline"
width = 46
height = 12
""")
        def fit(screen, label, rows):
            pane = self.frame(screen, contains=label)
            if not pane:
                return False
            row, col, height, width = pane
            body = screen.splitlines()[row + 1:row + height - 1]
            return height == rows + 2 and width == max(len(line[col + 1:col + width - 1].rstrip()) for line in body) + 2

        self.wait(lambda s: fit(s, 'tree-file', 1))
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        self.wait(lambda s: fit(s, 'small.txt   5 B', 2))
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        self.wait(lambda s: fit(s, 'small.txt   5 B', 2) and self.frame(s, contains='small.txt   5 B')[0] == 26)
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        self.wait(lambda s: fit(s, 'Modified:', 3))

    def test_inline_narrows_the_candidate_list_only_when_visible(self):
        self.start_popup("""[ui]
width = 85
[preview]
enabled = true
layout = "inline"
start_hidden = true
position = "right"
""")
        def menu_width(screen):
            match = re.search(r'╭(─+)╮', screen)
            return len(match[1]) + 2 if match else None

        self.wait(lambda s: menu_width(s) == 85)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-p')
        screen = self.wait(lambda s: self.frame(s) and 'MUST_RESTORE_A' in s)
        frame = self.frame(screen)
        menu = re.search(r'╭(─+)╮', screen.splitlines()[frame[0]])
        self.assertEqual(menu.end(), frame[1], screen)
        self.assertLess(menu_width(screen), 85)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-p')
        self.wait(lambda s: self.frame(s) is None and menu_width(s) == 85 and 'MUST_RESTORE_A' not in s)

    def test_layout_toggle_and_floating_maximum_height_across_terminal_resize(self):
        cols, rows = 100, 30
        self.start_popup("""[ui]
width = 95
[preview]
enabled = true
layout = "inline"
position = "right"
width = 32
height = 8
min_width = 32
adaptive_size = false
""", cols, rows)

        def wait_frame(check, title='pick-a.txt'):
            screen = self.wait(lambda s: (frame := self.frame(s, title)) is not None and check(frame))
            return self.frame(screen, title)

        inline = wait_frame(lambda frame: frame[2:] == (8, 32))
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        wait_frame(lambda frame: frame == inline, 'pick-b.txt')
        self.tmux('send-keys', '-t', 'test:0.0', 'C-p')
        wait_frame(lambda frame: frame == inline)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        wait_frame(lambda frame: frame == inline)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        wait_frame(lambda frame: frame == (rows - 8, cols - 32, 8, 32))

        def expanded(screen):
            frame, menu = self.frame(screen), self.frame(screen, None)
            if frame is None or menu is None:
                return False
            available_rows = rows - (menu[0] + menu[2] + 1)
            return frame == (rows - available_rows, cols - 32, available_rows, 32) and menu[3] == 95

        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        self.wait(expanded)
        cols, rows = 120, 40
        self.tmux('resize-window', '-t', 'test:0', '-x', str(cols), '-y', str(rows))
        self.wait(expanded)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        wait_frame(lambda frame: frame == (rows - 8, cols - 32, 8, 32))
        self.tmux('send-keys', '-t', 'test:0.0', 'M-p')
        self.wait(lambda s: self.frame(s) is None and 'pick-a.txt' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-f')
        self.wait(lambda s: self.frame(s) is None and 'pick-a.txt' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-p')
        wait_frame(lambda frame: frame[0] == inline[0] and frame[2:] == (8, 32))

    def test_floating_reopens_at_maximum_near_bottom_with_full_scrollback(self):
        with (self.home / '.zshrc').open('a') as rc:
            rc.write("histfill() { printf 'HISTORY_%03d\\n' {1..310}; }\n")
        self.start_popup("""[ui]
width = 100
[preview]
enabled = true
layout = "floating"
adaptive_size = false
""", 100, 34)
        self.wait(lambda s: self.frame(s) is not None and 'MUST_RESTORE_A' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-z')
        large = self.frame(self.wait(lambda s: (p := self.frame(s)) and p[2] > 20 and p[3] == 40))
        self.assertEqual(large[0] + large[2], 34)
        def close_and_clear():
            self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
            self.wait(lambda s: self.frame(s) is None)
            self.tmux('send-keys', '-t', 'test:0.0', 'C-u')
            return self.wait(lambda s: 'cat pick-' not in s and 'RIGHT_PROMPT' in s)

        close_and_clear()
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'histfill')
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait(lambda s: 'HISTORY_310' in s and 'RIGHT_PROMPT' in s)
        self.tmux('resize-window', '-t', 'test:0', '-x', '100', '-y', '22')
        baseline = self.wait(lambda s: len(s.splitlines()) == 22 and 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')

        def fills_the_top(screen):
            pane, menu = self.frame(screen), self.frame(screen, None)
            return pane and menu and pane == (0, 60, menu[0] - 1, 40)

        top = self.wait(fills_the_top)
        self.assertIn('RIGHT_PROMPT', top)
        self.assertIn('FIRST_PROMPT_LINE', top)
        restored = close_and_clear()
        self.assertEqual(restored, baseline)
        self.tmux('resize-window', '-t', 'test:0', '-x', '100', '-y', '34')
        self.tmux('send-keys', '-t', 'test:0.0', 'C-l')
        self.wait(lambda s: s.startswith('FIRST_PROMPT_LINE') and 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        self.wait(lambda s: self.frame(s) == large and 'MUST_RESTORE_A' in s)

    def test_floating_compacts_the_list_to_reveal_a_frame_then_restores_it(self):
        for i in range(6):
            (self.home / f'pick-extra-{i:02d}').touch()
        self.start_popup("""[ui]
width = 50
[preview]
enabled = true
layout = "floating"
start_hidden = true
""", 50, 9)
        original = (2, 0, 7, 50)
        self.wait(lambda s: self.frame(s, None) == original)
        self.tmux('send-keys', '-t', 'test:0.0', 'M-p')
        self.wait(lambda s: self.frame(s) == (6, 32, 3, 18) and self.frame(s, None) == (2, 0, 4, 50))
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        self.wait(lambda s: self.frame(s, 'pick-b.txt') == (6, 32, 3, 18))
        self.tmux('send-keys', '-t', 'test:0.0', 'M-p')
        self.wait(lambda s: self.frame(s, None) == original and self.frame(s, 'pick-b.txt') is None)

    def test_floating_changes_corner_on_resize_and_restores_the_old_rectangle(self):
        with (self.home / '.zshrc').open('a') as rc:
            rc.write("""if [[ -n $LIG_TTY ]]; then
    for i in {1..14}; do printf 'PRIOR_%02d%82s\n' "$i" RESTORE_EDGE; done
fi
""")
        self.tmux('new-session', '-d', '-x', '100', '-y', '25', '-s', 'test', '-c', str(self.home), ZSH, '-i')
        baseline = self.wait(lambda s: 'RIGHT_PROMPT' in s and 'PRIOR_14' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        top = self.wait(lambda s: self.frame(s) == (0, 82, 3, 18) and 'MUST_RESTORE_A' in s)
        self.assertNotEqual(top.splitlines()[0], baseline.splitlines()[0])
        self.tmux('resize-window', '-t', 'test:0', '-x', '100', '-y', '48')
        bottom = self.wait(lambda s: self.frame(s) == (45, 82, 3, 18) and 'MUST_RESTORE_A' in s)
        self.assertEqual(bottom.splitlines()[:14], baseline.splitlines()[:14])
        self.assertRegex(bottom, r'preview-test> cat pick- +RIGHT_PROMPT')
        self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
        restored = self.wait(lambda s: 'MUST_RESTORE' not in s and 'pick-a.txt' not in s)
        self.assertEqual(restored.splitlines()[:14], baseline.splitlines()[:14])

    def test_late_start_prefers_the_upper_half_and_a_clear_releases_the_top_edge(self):
        (self.home / 'config/lig/config.toml').write_text("""[preview]
enabled = true
layout = "floating"
width = 32
height = 6
adaptive_size = false
""")
        with (self.home / '.zshrc').open('a') as rc:
            rc.write("""if [[ -n $LIG_TTY ]]; then print -l START_ONE START_TWO START_THREE START_FOUR; fi
known_screen() { printf '\\033[2J\\033[37;1H'; }
""")
        self.tmux('new-session', '-d', '-x', '100', '-y', '45', '-s', 'test', '-c', str(self.home),
                  '/bin/sh', '-c', 'printf "\\033[4;70HPRE_LIG_OUTPUT\\033[33;1H"; exec "$0" -i', ZSH)
        self.wait(lambda s: 'PRE_LIG_OUTPUT' in s and 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        before = self.wait(lambda s: self.frame(s) is not None and 'MUST_RESTORE_A' in s)
        self.assertEqual(self.frame(before)[0], 32)
        self.assertIn('PRE_LIG_OUTPUT', before)
        self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
        self.wait(lambda s: self.frame(s) is None and 'START_ONE' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'C-u')
        self.wait(lambda s: 'cat pick-' not in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'known_screen')
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait(lambda s: 'PRE_LIG_OUTPUT' not in s and 'known_screen' not in s and 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        after = self.wait(lambda s: self.frame(s) == (0, 68, 6, 32) and 'MUST_RESTORE_A' in s)
        self.assertIn('RIGHT_PROMPT', after)

    def test_floating_preview_preserves_prompt_restores_screen_and_survives_resize(self):
        self.tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(self.home),
                  ZSH, '-i')
        self.wait(lambda s: 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        screen = self.wait(lambda s: 'MUST_RESTORE_A' in s)
        self.assertIn('FIRST_PROMPT_LINE', screen)
        self.assertRegex(screen, r'preview-test> cat pick- +RIGHT_PROMPT')
        self.tmux('resize-window', '-t', 'test:0', '-x', '120', '-y', '40')
        self.wait(lambda s: 'MUST_RESTORE_A' in s and 'RIGHT_PROMPT' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'Tab')
        self.wait(lambda s: 'MUST_RESTORE_B' in s and 'MUST_RESTORE_A' not in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
        screen = self.wait(lambda s: 'MUST_RESTORE' not in s and 'pick-a.txt' not in s)
        lines = [line for line in screen.splitlines() if line.strip()]
        self.assertEqual(len(lines), 2, lines)
        self.assertEqual(lines[0], 'FIRST_PROMPT_LINE')
        self.assertRegex(lines[1], r'preview-test> cat pick- +RIGHT_PROMPT')

    def test_upward_floating_preview_restores_overwritten_output(self):
        with (self.home / '.zshrc').open('a') as rc:
            rc.write("if [[ -n $LIG_TTY ]]; then printf 'PRIOR_%02d\\n' {1..45}; fi\n")
        self.tmux('new-session', '-d', '-x', '100', '-y', '24', '-s', 'test', '-c', str(self.home),
                  ZSH, '-i')
        baseline = self.wait(lambda s: 'RIGHT_PROMPT' in s and 'PRIOR_45' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        visible = self.wait(lambda s: 'MUST_RESTORE_A' in s)
        row = lambda screen, text: next(i for i, line in enumerate(screen.splitlines()) if text in line)
        self.assertLess(row(visible, 'MUST_RESTORE_A'), row(visible, 'FIRST_PROMPT_LINE'), visible)
        self.assertIn('RIGHT_PROMPT', visible)
        self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
        restored = self.wait(lambda s: 'MUST_RESTORE' not in s and 'pick-a.txt' not in s)
        self.assertEqual([l for l in baseline.splitlines() if l.startswith('PRIOR_')],
                         [l for l in restored.splitlines() if l.startswith('PRIOR_')])

    def test_wrapped_command_rows_survive_the_overlay(self):
        self.tmux('new-session', '-d', '-x', '80', '-y', '24', '-s', 'test', '-c', str(self.home),
                  ZSH, '-i')
        self.wait(lambda s: 'preview-test>' in s)
        command = 'cat operand_' + 'x' * 100 + ' pick-'
        self.tmux('send-keys', '-t', 'test:0.0', '-l', command)
        visible = self.wait(lambda s: 'MUST_RESTORE_A' in s)
        def input_rows(screen):
            rows = screen.splitlines()
            start = next(i for i, row in enumerate(rows) if row.startswith('preview-test> cat operand_'))
            return rows[start:start + 2]

        prompt = input_rows(visible)
        self.assertEqual(''.join(prompt), 'preview-test> ' + command)
        self.tmux('send-keys', '-t', 'test:0.0', 'Escape')
        restored = self.wait(lambda s: 'MUST_RESTORE' not in s and 'pick-a.txt' not in s)
        self.assertEqual(input_rows(restored), prompt)

    def test_resize_retains_independent_scroll(self):
        config = self.home / 'config/lig/config.toml'
        with config.open('a') as stream:
            stream.write('adaptive_size = false\nmin_height = 8\n')
        (self.home / 'pick-a.txt').write_text('\n'.join(MARKS))
        self.tmux('new-session', '-d', '-x', '100', '-y', '26', '-s', 'test', '-c', str(self.home),
                  ZSH, '-i')
        self.wait(lambda s: 'preview-test>' in s)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'cat pick-')
        self.wait(lambda s: 'ROW_01' in s)
        self.tmux('send-keys', '-t', 'test:0.0', 'PageDown')

        def first_row(screen):
            return next(iter(re.findall(r'ROW_\d+', screen)), None)

        scrolled = self.wait(lambda s: first_row(s) not in (None, 'ROW_01'))
        self.tmux('resize-window', '-t', 'test:0', '-x', '120', '-y', '28')
        self.wait(lambda s: first_row(s) == first_row(scrolled) and 'RIGHT_PROMPT' in s)



if __name__ == '__main__':
    unittest.main(verbosity=2)
