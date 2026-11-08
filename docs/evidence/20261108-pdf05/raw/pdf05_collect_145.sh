#!/usr/bin/env bash
# PDF-05：把 145 上产出的样例 PDF 抽成可核对的文本 / 元信息 / 渲染图
set -eu
cd /root/pdf05-evidence
mkdir -p raw sample
for f in sample/*.pdf; do
    name=$(basename "$f" .pdf)
    pdftotext -layout "$f" "raw/pdftotext_$name.txt"
    pdfinfo "$f" > "raw/pdfinfo_$name.txt"
    pdffonts "$f" > "raw/pdffonts_$name.txt"
    pdftoppm -r 100 -png -f 1 -l 1 "$f" "sample/$name-p1" >/dev/null 2>&1 || true
done
ls -la sample
echo COLLECT_DONE
