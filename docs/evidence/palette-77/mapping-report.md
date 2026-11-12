# gallery 77 套 → 云枢 5 槽位映射报告

- 数据源：`docs/design/palette-schemes-77.json`（77 套 / 264 个源色）
- 生成器：`scripts/verify/gen_palette_schemes.py`（本报告与 `palettes.ts` 的 gallery 段同源生成）
- 校验器：`python3 scripts/verify/check_palettes.py`

## 规则

| 源色数 | brand | brandLight | merge | border | accent |
|---|---|---|---|---|---|
| 4 | 最暗 | 次暗 | 最亮 | merge −8% L | 次亮 |
| 3 | 最暗 | brand +22% L | 最亮 | merge −8% L | 中间 |
| 2 | 最暗 | brand +22% L | 最亮 | merge −8% L | brand 补色 +180° |
| 1 | 该色 | brand +22% L | brand → L95 | brand → L82 | brand 补色 +180° |

源色一律原样使用（派生槽位单独标出）；merge 上正文（#1A1A1A / #F5F5F5）不足 4.5:1 时才微调 merge。

## 逐条映射

| # | id | 名称 | 分组 | 源色（按 L 升序） | brand | brandLight | merge | border | accent | 派生槽位 | merge 兜底 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `nat-canglan-naiyou` | 藏蓝奶油 | 时尚配色 | `#24364A` · `#F4E8DA` | `#24364A` | `#496D95` | `#F4E8DA` | `#EBD4BB` | `#4A3824` | brandLight、border、accent | — |
| 2 | `nat-yunhui-tuo` | 云灰驼 | 时尚配色 | `#C7B8A7` · `#E6E0D7` · `#ECECEC` | `#C7B8A7` | `#F3EFEC` | `#ECECEC` | `#D8D8D8` | `#E6E0D7` | brandLight、border | — |
| 3 | `nat-shenhai-wulan` | 深海雾蓝 | 时尚配色 | `#41647F` · `#94BBD2` | `#41647F` | `#779CB9` | `#94BBD2` | `#77A9C6` | `#7F5C41` | brandLight、border、accent | — |
| 4 | `guci-jiutao` | 粉彩九桃天球瓶 | 古典釉色 | `#B02A46` · `#8FA878` · `#E8E1D6` | `#B02A46` | `#DC6E85` | `#E8E1D6` | `#D9CEBC` | `#8FA878` | brandLight、border | — |
| 5 | `guci-hulu` | 金地粉彩缠枝莲葫芦瓶 | 古典釉色 | `#0D3A91` · `#D4B24F` · `#E8E1D6` | `#0D3A91` | `#2367EB` | `#E8E1D6` | `#D9CEBC` | `#D4B24F` | brandLight、border | — |
| 6 | `guci-zhuangxin` | 青花游鱼转心瓶 | 古典釉色 | `#0D3A91` · `#7793C3` · `#E8E1D6` | `#0D3A91` | `#2367EB` | `#E8E1D6` | `#D9CEBC` | `#7793C3` | brandLight、border | — |
| 7 | `guci-zhuangxin-2` | 青花游鱼转心瓶·龙纹 | 古典釉色 | `#B02A1F` · `#6F8EC7` · `#E8E1D6` | `#B02A1F` | `#E2675D` | `#E8E1D6` | `#D9CEBC` | `#6F8EC7` | brandLight、border | — |
| 8 | `guci-wanhua` | 粉彩万花葫芦瓶 | 古典釉色 | `#81879F` · `#D55B67` · `#EB9075` | `#81879F` | `#C1C4D0` | `#EB9075` | `#E67351` | `#D55B67` | brandLight、border | — |
| 9 | `guci-huaniao` | 青花粉彩花鸟扁瓶 | 古典釉色 | `#0D3A91` · `#8DA39C` · `#D7A1B6` · `#E8E1D6` | `#0D3A91` | `#8DA39C` | `#E8E1D6` | `#D9CEBC` | `#D7A1B6` | border | — |
| 10 | `guci-meiping` | 元青花四爱图梅瓶 | 古典釉色 | `#0D3A91` · `#7793C3` · `#E8E1D6` | `#0D3A91` | `#2367EB` | `#E8E1D6` | `#D9CEBC` | `#7793C3` | brandLight、border | — |
| 11 | `guci-fushou` | 粉彩镂空福寿双耳瓶 | 古典釉色 | `#B7372D` · `#D4B13D` · `#F0EADF` | `#B7372D` | `#DD7E77` | `#F0EADF` | `#E3D8C3` | `#D4B13D` | brandLight、border | — |
| 12 | `nat-dizhonghai` | 地中海红蓝 | 时尚配色 | `#1A1A1A` · `#0E4A7B` · `#C13320` · `#F3E8DA` | `#1A1A1A` | `#0E4A7B` | `#F3E8DA` | `#E9D5BB` | `#C13320` | border | — |
| 13 | `guci-qingluan` | 晴峦叠翠 | 古典釉色 | `#4F7D78` · `#6C8A69` · `#A8AAA1` | `#4F7D78` | `#88B4AF` | `#A8AAA1` | `#94978C` | `#6C8A69` | brandLight、border | — |
| 14 | `guci-qianli` | 青绿千里 | 古典釉色 | `#77939D` · `#8BB6B1` · `#D4C5AA` | `#77939D` | `#B8C7CC` | `#D4C5AA` | `#C6B38F` | `#8BB6B1` | brandLight、border | — |
| 15 | `guci-yuanshan` | 远山入黛 | 古典釉色 | `#40566B` · `#657B74` · `#D6D8D4` | `#40566B` | `#718EAA` | `#D6D8D4` | `#C2C5BF` | `#657B74` | brandLight、border | — |
| 16 | `nat-danxia` | 丹霞赭石 | 时尚配色 | `#9B5E45` · `#C46B52` · `#D1A056` | `#9B5E45` | `#C99A87` | `#D1A056` | `#C88E36` | `#C46B52` | brandLight、border | — |
| 17 | `pms-sports-outdoor` | 咖啡里昂蓝 | 时尚配色 | `#492610` · `#175973` · `#AB8570` · `#BFC6CE` | `#492610` | `#175973` | `#BFC6CE` | `#A8B1BC` | `#AB8570` | border | — |
| 18 | `pms-popular` | 沙灰肉桂 | 时尚配色 | `#593833` · `#93755D` · `#6FC4EB` · `#E4C9AE` | `#593833` | `#93755D` | `#E4C9AE` | `#DAB58F` | `#6FC4EB` | border | — |
| 19 | `pms-sports-outdoor-2` | 灰紫波塞冬 | 时尚配色 | `#062D4C` · `#8F8176` · `#AC8F8B` · `#D5CDCA` | `#062D4C` | `#8F8176` | `#D5CDCA` | `#C3B8B3` | `#AC8F8B` | border | — |
| 20 | `pms-sports-outdoor-3` | 早霜阿拉斯加 | 时尚配色 | `#35140F` · `#79AED8` · `#C1B198` · `#DCD9D2` | `#35140F` | `#79AED8` | `#DCD9D2` | `#CAC6BB` | `#C1B198` | border | — |
| 21 | `pms-sports-outdoor-4` | 榛子翠蓝 | 时尚配色 | `#1C3860` · `#6A615C` · `#CDAA8C` · `#DCDCDC` | `#1C3860` | `#6A615C` | `#DCDCDC` | `#C8C8C8` | `#CDAA8C` | border | — |
| 22 | `pms-shuangzi` | 霜紫宇宙紫 | 时尚配色 | `#5A425C` · `#9E887B` · `#B2A4B5` · `#D7DBDC` | `#5A425C` | `#9E887B` | `#D7DBDC` | `#C1C7C9` | `#B2A4B5` | border | — |
| 23 | `pms-chengbao` | 城堡波特酒 | 时尚配色 | `#582933` · `#6B484E` · `#73A5C8` · `#BBB2A9` | `#582933` | `#6B484E` | `#BBB2A9` | `#A99E92` | `#73A5C8` | border | — |
| 24 | `pms-xinchao` | 新潮空间 | 时尚配色 | `#56352E` · `#3C5F73` · `#B0835A` · `#DFCCBE` | `#56352E` | `#3C5F73` | `#DFCCBE` | `#D2B7A3` | `#B0835A` | border | — |
| 25 | `pms-sports-outdoor-5` | 海洋洞穴 | 时尚配色 | `#1C203D` · `#908571` · `#A7B9CD` · `#F2B08E` | `#1C203D` | `#908571` | `#F2B08E` | `#EE9669` | `#A7B9CD` | border | — |
| 26 | `nat-img9151` | 色卡 9151 | 时尚配色 | `#A45668` · `#D59BA8` · `#C7C1B5` · `#E7DDD3` | `#A45668` | `#D59BA8` | `#E7DDD3` | `#D9C9B9` | `#C7C1B5` | border | — |
| 27 | `nat-img9152` | 色卡 9152 | 时尚配色 | `#8C2F2D` · `#8E8075` · `#B76F6A` · `#B7C5B7` | `#8C2F2D` | `#8E8075` | `#B7C5B7` | `#A0B3A0` | `#B76F6A` | border | — |
| 28 | `nat-img9153` | 色卡 9153 | 时尚配色 | `#4F6072` · `#B56A58` · `#8DB8A7` · `#D9B07C` | `#4F6072` | `#B56A58` | `#D9B07C` | `#D09D5C` | `#8DB8A7` | border | — |
| 29 | `nat-img9154` | 色卡 9154 | 时尚配色 | `#607E95` · `#B8AEA6` · `#A8C3D6` · `#E2D0BC` | `#607E95` | `#B8AEA6` | `#E2D0BC` | `#D6BCA0` | `#A8C3D6` | border | — |
| 30 | `nat-img9155` | 色卡 9155 | 时尚配色 | `#5B6466` · `#8C9B9F` · `#C6C9C0` · `#DFE2DA` | `#5B6466` | `#8C9B9F` | `#DFE2DA` | `#CBD0C3` | `#C6C9C0` | border | — |
| 31 | `nat-img9156` | 色卡 9156 | 时尚配色 | `#B7C4CF` · `#D1D8DC` · `#E8E9E6` · `#F1F2EE` | `#B7C4CF` | `#D1D8DC` | `#F1F2EE` | `#DEE0D7` | `#E8E9E6` | border | — |
| 32 | `nat-img9157` | 色卡 9157 | 时尚配色 | `#4A6460` · `#7A9387` · `#BFD0B8` · `#DDE6D6` | `#4A6460` | `#7A9387` | `#DDE6D6` | `#C8D7BD` | `#BFD0B8` | border | — |
| 33 | `nat-img9158` | 色卡 9158 | 时尚配色 | `#2E6A67` · `#5E8C87` · `#A9C8BE` · `#D7E6DB` | `#2E6A67` | `#5E8C87` | `#D7E6DB` | `#BED6C4` | `#A9C8BE` | border | — |
| 34 | `nat-img9159` | 色卡 9159 | 时尚配色 | `#7E6554` · `#A78A73` · `#D4B79D` · `#E9D6BE` | `#7E6554` | `#A78A73` | `#E9D6BE` | `#DFC3A0` | `#D4B79D` | border | — |
| 35 | `guci-youlibai` | 釉里白 | 古典釉色 | `#315F86` · `#B7BEC3` · `#F2EEE5` | `#315F86` | `#6298C5` | `#F2EEE5` | `#E4DCCA` | `#B7BEC3` | brandLight、border | — |
| 36 | `guci-ciyun` | 青花瓷韵 | 古典釉色 | `#5F817B` · `#9CB4AE` · `#EEF1ED` | `#5F817B` | `#9BB5B1` | `#EEF1ED` | `#D8DFD6` | `#9CB4AE` | brandLight、border | — |
| 37 | `guci-haishui` | 海水江崖 | 古典釉色 | `#244A73` · `#9CC7CF` · `#F5F2EA` | `#244A73` | `#4381C4` | `#F5F2EA` | `#E8E1CE` | `#9CC7CF` | brandLight、border | — |
| 38 | `guci-yingyue` | 夜瓷映月 | 古典釉色 | `#263D46` · `#B8BDC2` · `#E7E3D8` | `#263D46` | `#4D7C8F` | `#E7E3D8` | `#D7D1BF` | `#B8BDC2` | brandLight、border | — |
| 39 | `nat-wulan-qing` | 浅雾蓝青 | 时尚配色 | `#2F787D` · `#4F5F65` · `#D8E6EF` | `#2F787D` | `#5BBBC1` | `#D8E6EF` | `#BBD3E3` | `#4F5F65` | brandLight、border | — |
| 40 | `nat-img9165` | 色卡 9165 | 时尚配色 | `#800000` · `#982B1C` · `#DAD4B5` | `#800000` | `#F00000` | `#DAD4B5` | `#CCC49A` | `#982B1C` | brandLight、border | — |
| 41 | `nat-img9166` | 色卡 9166 | 时尚配色 | `#4A503D` · `#8E9775` · `#FAF2DA` | `#4A503D` | `#85906E` | `#FAF2DA` | `#F5E5B6` | `#8E9775` | brandLight、border | — |
| 42 | `nat-img9167` | 色卡 9167 | 时尚配色 | `#DD4A48` · `#97BFB4` · `#F5EEDC` | `#DD4A48` | `#EFA8A7` | `#F5EEDC` | `#ECDFBC` | `#97BFB4` | brandLight、border | — |
| 43 | `nat-img9168` | 色卡 9168 | 时尚配色 | `#A20A0A` · `#FFA36C` · `#F6EEC9` | `#A20A0A` | `#F22A2A` | `#F6EEC9` | `#F0E3A6` | `#FFA36C` | brandLight、border | — |
| 44 | `nat-img9169` | 色卡 9169 | 时尚配色 | `#AA3A3A` · `#A4B787` · `#F4F4F4` | `#AA3A3A` | `#D48080` | `#F4F4F4` | `#E0E0E0` | `#A4B787` | brandLight、border | — |
| 45 | `nat-img9170` | 色卡 9170 | 时尚配色 | `#A31D1D` · `#E5D0AC` · `#FEF9E1` | `#A31D1D` | `#E05050` | `#FEF9E1` | `#FDF1BA` | `#E5D0AC` | brandLight、border | — |
| 46 | `pms-sports-outdoor-6` | 羚羊皮翠蓝 | 时尚配色 | `#1C3961` · `#CB5E33` · `#F4B06F` · `#EFDECE` | `#1C3961` | `#CB5E33` | `#EFDECE` | `#E5C9AF` | `#F4B06F` | border | — |
| 47 | `pms-sports-outdoor-7` | 橡金橘探戈 | 时尚配色 | `#8C5F4C` · `#B36D28` · `#E94C2D` · `#BFA475` | `#8C5F4C` | `#B36D28` | `#BFA475` | `#B29259` | `#E94C2D` | border | — |
| 48 | `pms-shanye` | 到山野间 | 时尚配色 | `#372E2F` · `#5E6883` · `#B48F87` · `#BDB17F` | `#372E2F` | `#5E6883` | `#BDB17F` | `#AFA164` | `#B48F87` | border | — |
| 49 | `pms-sports-outdoor-8` | 方钠石金猕猴 | 时尚配色 | `#143062` · `#8E565F` · `#685DA1` · `#EFDA3F` | `#143062` | `#8E565F` | `#EFDA3F` | `#ECD319` | `#685DA1` | border | — |
| 50 | `pms-migua` | 蜜瓜色 | 时尚配色 | `#DDDDDD` | `#DDDDDD` | `#FFFFFF` | `#F2F2F2` | `#D1D1D1` | `#DDDDDD` | brandLight、merge、border、accent | — |
| 51 | `pms-sports-outdoor-9` | 瓦蓝三色堇 | 时尚配色 | `#197F8B` · `#5E3C78` · `#D7C094` · `#EE94B0` | `#197F8B` | `#5E3C78` | `#EE94B0` | `#E87196` | `#D7C094` | border | — |
| 52 | `pms-shenshuilan` | 深水蓝 | 时尚配色 | `#DDDDDD` | `#DDDDDD` | `#FFFFFF` | `#F2F2F2` | `#D1D1D1` | `#DDDDDD` | brandLight、merge、border、accent | — |
| 53 | `pms-yueliang` | 月亮蓝 | 时尚配色 | `#594D33` · `#3890A6` · `#CB394C` · `#E6E5E0` | `#594D33` | `#3890A6` | `#E6E5E0` | `#D4D2C9` | `#CB394C` | border | — |
| 54 | `pms-miaomiao` | 妙妙王国 | 时尚配色 | `#975891` · `#656692` · `#EEC933` · `#A9C8CA` | `#975891` | `#656692` | `#A9C8CA` | `#90B8BA` | `#EEC933` | border | — |
| 55 | `pms-kaoguo` | 烤果馅饼 | 时尚配色 | `#DDDDDD` | `#DDDDDD` | `#FFFFFF` | `#F2F2F2` | `#D1D1D1` | `#DDDDDD` | brandLight、merge、border、accent | — |
| 56 | `pms-xingrenlao` | 杏仁酪 | 时尚配色 | `#3E685C` · `#A3664A` · `#889DA2` · `#E9BB9A` | `#3E685C` | `#A3664A` | `#E9BB9A` | `#E2A478` | `#889DA2` | border | — |
| 57 | `pms-heimei` | 黑莓果酱 | 时尚配色 | `#2C0A23` · `#2B2855` · `#A38D82` · `#E1CC93` | `#2C0A23` | `#2B2855` | `#E1CC93` | `#D8BD73` | `#A38D82` | border | — |
| 58 | `pms-sports-outdoor-10` | 褐菇海洋洞穴 | 时尚配色 | `#382618` · `#142747` · `#92745C` · `#F1F4FB` | `#382618` | `#142747` | `#F1F4FB` | `#D1DBF2` | `#92745C` | border | — |
| 59 | `pms-hejin` | 时尚合金 | 时尚配色 | `#A66D5A` · `#C5BAA6` · `#DBB994` · `#D1D4DB` | `#A66D5A` | `#C5BAA6` | `#D1D4DB` | `#BABFC9` | `#DBB994` | border | — |
| 60 | `pms-shanchuang` | 山创空间 | 时尚配色 | `#4978AE` · `#D6A850` · `#DCD785` · `#C5C5C5` | `#4978AE` | `#D6A850` | `#C5C5C5` | `#B1B1B1` | `#DCD785` | border | — |
| 61 | `pms-sports-outdoor-11` | 云米山鹑 | 时尚配色 | `#71523D` · `#7F848A` · `#B99A6E` · `#E8DFC0` | `#71523D` | `#7F848A` | `#E8DFC0` | `#DDD0A2` | `#B99A6E` | border | — |
| 62 | `pms-dama` | 大麻色 | 时尚配色 | `#223B1D` · `#8F8639` · `#BFAE80` · `#DED0B5` | `#223B1D` | `#8F8639` | `#DED0B5` | `#D1BE99` | `#BFAE80` | border | — |
| 63 | `pms-shanxi` | 山系色会 | 时尚配色 | `#DDDDDD` | `#DDDDDD` | `#FFFFFF` | `#F2F2F2` | `#D1D1D1` | `#DDDDDD` | brandLight、merge、border、accent | — |
| 64 | `nat-persimmon` | 柿子色 | 时尚配色 | `#8A9272` · `#D96B37` · `#ED9D65` · `#F2DFC5` | `#8A9272` | `#D96B37` | `#F2DFC5` | `#EBCDA4` | `#ED9D65` | border | — |
| 65 | `nat-crested-ibis` | 朱鹮 | 时尚配色 | `#A6B5AF` · `#D9A5A4` · `#EACAC0` · `#F2E8DA` | `#A6B5AF` | `#D9A5A4` | `#F2E8DA` | `#E7D5BC` | `#EACAC0` | border | — |
| 66 | `nat-bee-eater` | 蓝喉蜂虎 | 时尚配色 | `#3F838B` · `#B57B50` · `#8DAD6E` · `#77B7B4` | `#3F838B` | `#B57B50` | `#77B7B4` | `#5CA9A5` | `#8DAD6E` | border | — |
| 67 | `nat-osmanthus` | 桂花 | 时尚配色 | `#697D58` · `#E8B957` · `#A9B491` · `#F4DFC0` | `#697D58` | `#E8B957` | `#F4DFC0` | `#EECD9D` | `#A9B491` | border | — |
| 68 | `nat-muhly-grass` | 芒草 | 时尚配色 | `#A7697D` · `#D2A0AD` · `#E8CDD2` · `#EAE5D9` | `#A7697D` | `#D2A0AD` | `#EAE5D9` | `#DBD3BF` | `#E8CDD2` | border | — |
| 69 | `nat-huxiu` | 狐锈红 | 时尚配色 | `#B95E3F` · `#A5A18C` · `#D59670` · `#F0D9B9` | `#B95E3F` | `#A5A18C` | `#F0D9B9` | `#E9C797` | `#D59670` | border | — |
| 70 | `nat-oak` | 橡木 | 时尚配色 | `#916449` · `#BD9669` · `#B2B39A` · `#DEC7A3` | `#916449` | `#BD9669` | `#DEC7A3` | `#D3B585` | `#B2B39A` | border | — |
| 71 | `nat-maple` | 枫 | 时尚配色 | `#A34D49` · `#CE7956` · `#E6B98A` · `#DCD5C7` | `#A34D49` | `#CE7956` | `#DCD5C7` | `#CCC2AE` | `#E6B98A` | border | — |
| 72 | `nat-chanterelle` | 鸡油菌 | 时尚配色 | `#929B69` · `#D19A40` · `#E8BF6A` · `#ECDDC1` | `#929B69` | `#D19A40` | `#ECDDC1` | `#E2CCA2` | `#E8BF6A` | border | — |
| 73 | `guci-fenqiang` | 粉墙桂蕊 | 古典釉色 | `#70806B` · `#D8A847` · `#F3EFE4` | `#70806B` | `#AAB5A6` | `#F3EFE4` | `#E6DEC8` | `#D8A847` | brandLight、border | — |
| 74 | `guci-chishui` | 池水太湖 | 古典釉色 | `#B95C43` · `#9DBBB5` · `#D6D0C3` | `#B95C43` | `#D8A294` | `#D6D0C3` | `#C5BDAB` | `#9DBBB5` | brandLight、border | — |
| 75 | `guci-xuanzhi` | 宣纸晚霞 | 古典釉色 | `#697553` · `#E5A360` · `#F2E7D3` | `#697553` | `#A1AD8B` | `#F2E7D3` | `#E9D6B4` | `#E5A360` | brandLight、border | — |
| 76 | `guci-yuedong` | 月洞桂香 | 古典釉色 | `#486E73` · `#D4A64C` · `#E8E2D6` | `#486E73` | `#7DA8AE` | `#E8E2D6` | `#D9D0BC` | `#D4A64C` | brandLight、border | — |
| 77 | `guci-gongming` | 暮雪宫门 | 古典釉色 | `#8C3D34` · `#D7A35A` · `#D8DADF` | `#8C3D34` | `#C77369` | `#D8DADF` | `#C2C5CD` | `#D7A35A` | brandLight、border | — |

## 汇总

- 方案数：77（古典釉色 20 · 时尚配色 57）
- 源色总数：264（1 色 4 · 2 色 2 · 3 色 28 · 4 色 43）
- merge 对比度兜底触发：0 套
- 数据疑点：第 50/52/55/63 套源数据只有一个色值且都是 `#DDDDDD`（色名分别为蜜瓜色 / 深水蓝 / 烤果馅饼 / 树皮色，PANTONE 参考号各不相同）—— 判断为提取件的占位色而非真实取色。按「色值原样保留」红线未擅自改值，等需求方提供真实色值后替换 JSON 重跑生成器即可。
