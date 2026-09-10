#!/usr/bin/env python3
"""Bake Inter text/wordmark alpha masks for the small static status renderer."""
import argparse
import io
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from PIL import Image, ImageDraw, ImageFont


def face(path, weight, size):
    font = instantiateVariableFont(TTFont(path), {'wght': weight, 'opsz': 32}, inplace=True)
    font.flavor = None
    buffer = io.BytesIO()
    font.save(buffer)
    buffer.seek(0)
    return ImageFont.truetype(buffer, size)


def build(font, output):
    regular = face(font, 500, 28)
    atlas = Image.new('L', (32 * 96, 40))
    advances = []
    for i in range(96):
        char = chr(i + 32)
        glyph = Image.new('L', (32, 40))
        ImageDraw.Draw(glyph).text((0, 0), char, font=regular, fill=255)
        atlas.paste(glyph, (i * 32, 0))
        advances.append(round(regular.getlength(char)))
    bold = face(font, 800, 64)
    logo = Image.new('L', (400, 100))
    draw = ImageDraw.Draw(logo)
    x = 0
    for i, char in enumerate('couch.'):
        draw.text((x, 0), char, font=bold, fill=255)
        advance = bold.getlength(char)
        if i < 5:
            advance = bold.getlength('couch.'[i:i+2]) - bold.getlength('couch.'[i+1])
        x += advance - 2
    logo = logo.crop(logo.getbbox())
    arrays = [('font_alpha', atlas.tobytes()), ('font_advance', bytes(advances)), ('wordmark_alpha', logo.tobytes())]
    text = f'#define WORDMARK_W {logo.width}\n#define WORDMARK_H {logo.height}\n'
    for name, data in arrays:
        text += f'static const unsigned char {name}[] = {{\n'
        text += '\n'.join(','.join(str(v) for v in data[i:i+40]) + ',' for i in range(0, len(data), 40))
        text += '\n};\n'
    output.write_text(text)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--font', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    build(args.font, args.output)
