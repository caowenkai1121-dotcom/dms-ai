# 造「静默丢内容」那三类的夹具，专门用来验 _p_image / _p_pdf 的逐帧、逐页补漏。
#
# 为什么要它：这三类缺陷的共同形态是 **HTTP 200 + 少了内容**，
# 靠肉眼看回答看不出来 —— 判据必须是「某个只出现在第 2 帧/第 2 页的数字有没有进块里」。
# 每份夹具都埋一个**唯一 token**，断言直接找那个 token。
#
# 用法（容器内）：python /app/make_silent_fixtures.py /kbdata/_silent /hostfonts/simhei.ttf
import sys
from pathlib import Path

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '/kbdata/_silent')
FONT = sys.argv[2] if len(sys.argv) > 2 else '/hostfonts/simhei.ttf'
OUT.mkdir(parents=True, exist_ok=True)


def img(lines, w=1100, h=260):
    """白底黑字印刷体，够 tesseract 稳定识别（实测 34px 全对）"""
    from PIL import Image, ImageDraw, ImageFont
    im = Image.new('RGB', (w, h), 'white')
    d = ImageDraw.Draw(im)
    f = ImageFont.truetype(FONT, 34)
    for i, t in enumerate(lines):
        d.text((30, 30 + i * 60), t, fill='black', font=f)
    return im


def multi_frame_tif():
    """2 帧 TIFF：唯一 token `TIFFPAGE2-7788` **只在第 2 帧**。
    原实现只 OCR 第 0 帧 → 这个 token 不会出现在任何块里，而 HTTP 仍是 200。"""
    p = OUT / 'multiframe.tif'
    a = img(['多帧扫描件 第一页', '这一页只有标题'])
    b = img(['第二页 正文', '单次上限 TIFFPAGE2-7788 元'])
    a.save(p, save_all=True, append_images=[b], compression='tiff_deflate')
    return p


def mixed_pdf():
    """2 页 PDF：页 1 有真文本层，页 2 只有一张图（唯一 token `PDFOCR2-9911` 在图上）。
    原实现只在「整份无文本」才失败 → 页 2 静默丢掉。"""
    import fitz
    p = OUT / 'mixed.pdf'
    doc = fitz.open()
    pg = doc.new_page()
    # 文本层用 fitz 自带的 CJK 字体（Windows 字体在容器里只有挂进来的那一个）
    pg.insert_font(fontname='F0', fontfile=FONT)
    pg.insert_text((60, 100), '第一页 有文本层 TEXTPAGE1-5566', fontname='F0', fontsize=20)
    png = OUT / '_p2.png'
    img(['第二页 只有图像', '补贴 PDFOCR2-9911 元']).save(png)
    doc.new_page().insert_image(fitz.Rect(40, 40, 800, 240), filename=str(png))
    doc.save(p)
    doc.close()
    png.unlink()
    return p


def scanned_pdf():
    """整份扫描（无文本层），唯一 token `SCANONLY-3344`。
    逐页补 OCR 之后它应该能入库；OCR 不可用时必须 422 no_text_layer。"""
    import fitz
    p = OUT / 'scanned.pdf'
    png = OUT / '_s1.png'
    img(['整份扫描件', '限额 SCANONLY-3344 元']).save(png)
    doc = fitz.open()
    doc.new_page().insert_image(fitz.Rect(40, 40, 800, 240), filename=str(png))
    doc.save(p)
    doc.close()
    png.unlink()
    return p


if __name__ == '__main__':
    for f in (multi_frame_tif(), mixed_pdf(), scanned_pdf()):
        print(f'✅ {f}  {f.stat().st_size} B', flush=True)
