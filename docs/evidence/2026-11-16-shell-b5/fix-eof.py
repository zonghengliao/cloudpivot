#!/usr/bin/env python3
"""去掉文件末尾多余的空行（shell.less 结尾多了一个空行，`git diff --check` 报 EOF blank line）。"""
import sys

p = '/opt/cloudpivot/frontend/src/style/shell.less'
s = open(p, encoding='utf-8').read()
s = s.rstrip('\n') + '\n'
open(p, 'w', encoding='utf-8').write(s)
print(repr(open(p, encoding='utf-8').read()[-30:]))
