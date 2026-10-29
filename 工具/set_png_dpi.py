"""把 PNG 的解析度標記改成指定 DPI（預設 300），像素不動。

Chrome 截圖寫的是 72／96 DPI 或不寫，檔案內容的像素已經是 300 DPI 的量，只差標記。
用法：python tools/set_png_dpi.py 檔案.png [--dpi 300]
"""

import argparse

from PIL import Image


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("png")
    ap.add_argument("--dpi", type=int, default=300)
    args = ap.parse_args()
    with Image.open(args.png) as im:
        im.load()
        im.save(args.png, dpi=(args.dpi, args.dpi), optimize=True)
    with Image.open(args.png) as im:
        print(f"{args.png}：{im.size[0]}×{im.size[1]} px，dpi={im.info.get('dpi')}")


if __name__ == "__main__":
    main()
