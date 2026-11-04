import glob, re, os
need = ['一、这个功能干什么', '二、谁能用', '三、完整操作步骤', '四、常见错误怎么办', '五、与哪些功能联动']
ok = True
for f in sorted(glob.glob('docs/manual/[0-9][0-9]-*.md')):
    txt = open(f, encoding='utf-8').read()
    miss = [s for s in need if s not in txt]
    imgs = len(re.findall(r'!\[[^\]]*\]\(', txt))
    steps = len(re.findall(r'^### 步骤', txt, re.M))
    lines = txt.count('\n') + 1
    print(f"{os.path.basename(f):24s} 行数={lines:4d} 截图={imgs} 步骤={steps} 缺失章节={miss or '无'}")
    ok = ok and not miss and imgs > 0 and steps > 0
print('\n结构全部合格' if ok else '\n有不合格项')
raise SystemExit(0 if ok else 1)
