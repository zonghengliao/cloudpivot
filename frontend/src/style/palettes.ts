/**
 * 配色方案引擎（6 套色板 × 明暗双模式）
 *
 * 设计目标：**不为每套方案手写一份 theme.less**。
 * 每套方案只声明 5 个基准色（主色 / 浅色 / 交融色 / 渐染色 / 强调色），
 * 其余所有变量（brand 色阶 1~10、中性容器色、文字色、图表色）都在运行时推导，
 * 最终以 CSS 变量形式注入 `<html>` 的内联 style —— 内联样式优先级高于任何选择器，
 * 因此能压过 TDesign 的 `:root[theme-mode='light'|'dark']` 块（theme.less 顶部注释记的坑）。
 *
 * 变量名严格复用现有约定（`--td-brand-color-1..10` / `--td-bg-color-*` / `--td-text-color-*`
 * / `--td-chart-color-*` / `--app-*`），现有组件零改动即可换肤。
 *
 * ⚠ 色值红线：PALETTES 里的色值来自需求方色表（云枢精选 22 套 + gallery 提取 77 套），
 *   **原样保留，不"优化"**。gallery 方案只有 1~4 个源色，未覆盖的槽位按亮度排序派生
 *   （提亮 / 压暗 / 补色），派生规则与逐条映射结果见 `docs/evidence/palette-77/mapping-report.md`；
 *   源色本身原样写在 `sourceColors` 里，一条都不丢。
 *   推导色阶、自动修正都是额外派生的变量，基准色本身永远不覆写。
 */

/** 明暗模式 */
export type ColorMode = 'light' | 'dark';

/** 方案分组：设置页分区展示用（featured = 云枢精选，即底座自带 22 套） */
export type PaletteGroup = 'featured' | 'porcelain' | 'fashion';

/** 云枢精选方案 id（底座自带 22 套） */
export type FeaturedPaletteId =
  | 'mist-blue'
  | 'deep-sea'
  | 'teal-sunset'
  | 'rose-gold'
  | 'violet-honey'
  | 'jade-crimson'
  | 'glaze-teal'
  | 'inkberry'
  | 'clear-sky'
  | 'peach-sky'
  | 'cream-celadon'
  | 'celadon-mist'
  | 'spring-sugar'
  | 'coral-bay'
  | 'peacock'
  | 'amber'
  | 'jade'
  | 'rouge'
  | 'grape'
  | 'moon-white'
  | 'clear-mist'
  | 'deep-blue-orange';

/** gallery 提取方案 id（77 套，由 scripts/verify/gen_palette_schemes.py 生成，勿手改） */
export type GalleryPaletteId =
  // <gallery-ids:begin>
  | 'nat-canglan-naiyou'
  | 'nat-yunhui-tuo'
  | 'nat-shenhai-wulan'
  | 'guci-jiutao'
  | 'guci-hulu'
  | 'guci-zhuangxin'
  | 'guci-zhuangxin-2'
  | 'guci-wanhua'
  | 'guci-huaniao'
  | 'guci-meiping'
  | 'guci-fushou'
  | 'nat-dizhonghai'
  | 'guci-qingluan'
  | 'guci-qianli'
  | 'guci-yuanshan'
  | 'nat-danxia'
  | 'pms-sports-outdoor'
  | 'pms-popular'
  | 'pms-sports-outdoor-2'
  | 'pms-sports-outdoor-3'
  | 'pms-sports-outdoor-4'
  | 'pms-shuangzi'
  | 'pms-chengbao'
  | 'pms-xinchao'
  | 'pms-sports-outdoor-5'
  | 'nat-img9151'
  | 'nat-img9152'
  | 'nat-img9153'
  | 'nat-img9154'
  | 'nat-img9155'
  | 'nat-img9156'
  | 'nat-img9157'
  | 'nat-img9158'
  | 'nat-img9159'
  | 'guci-youlibai'
  | 'guci-ciyun'
  | 'guci-haishui'
  | 'guci-yingyue'
  | 'nat-wulan-qing'
  | 'nat-img9165'
  | 'nat-img9166'
  | 'nat-img9167'
  | 'nat-img9168'
  | 'nat-img9169'
  | 'nat-img9170'
  | 'pms-sports-outdoor-6'
  | 'pms-sports-outdoor-7'
  | 'pms-shanye'
  | 'pms-sports-outdoor-8'
  | 'pms-migua'
  | 'pms-sports-outdoor-9'
  | 'pms-shenshuilan'
  | 'pms-yueliang'
  | 'pms-miaomiao'
  | 'pms-kaoguo'
  | 'pms-xingrenlao'
  | 'pms-heimei'
  | 'pms-sports-outdoor-10'
  | 'pms-hejin'
  | 'pms-shanchuang'
  | 'pms-sports-outdoor-11'
  | 'pms-dama'
  | 'pms-shanxi'
  | 'nat-persimmon'
  | 'nat-crested-ibis'
  | 'nat-bee-eater'
  | 'nat-osmanthus'
  | 'nat-muhly-grass'
  | 'nat-huxiu'
  | 'nat-oak'
  | 'nat-maple'
  | 'nat-chanterelle'
  | 'guci-fenqiang'
  | 'guci-chishui'
  | 'guci-xuanzhi'
  | 'guci-yuedong'
  | 'guci-gongming'
  // <gallery-ids:end>
  ;

/** 方案 id（写入 localStorage，勿随意改 —— 已发布的旧 id 必须永久保留） */
export type PaletteId = FeaturedPaletteId | GalleryPaletteId;

/** 一套方案的基准色（5 个色值 + 文案） */
export interface PaletteDef {
  /** 稳定 id，持久化用 */
  id: PaletteId;
  /** 方案中文名 */
  name: string;
  /** 方案来源/气质说明，用于设置页副标题 */
  note: string;
  /** ① 主色 = brand-7 */
  brand: string;
  /** ② 浅色 = brand-5 */
  brandLight: string;
  /** ③ 交融色 = 页面底色 */
  merge: string;
  /** ④ 渐染色 = 边框 */
  border: string;
  /** ⑤ 强调色 accent */
  accent: string;
  /** 分组（缺省视为 featured；设置页按分组分区展示） */
  group?: PaletteGroup;
  /** 原始来源文件（gallery 提取件，如 IMG_9129.JPG），设置页搜索用 */
  source?: string;
  /** 原始色值（未映射的源色，仅 gallery 方案有；映射规则见 docs/evidence/palette-77/mapping-report.md） */
  sourceColors?: readonly string[];
}

/**
 * 云枢精选二十二套（8 套初版 + 釉青/墨莓 + 14 套新增）。
 *
 * 色块语义：
 * - brand   → `--td-brand-color-7`，主按钮 / 选中态 / 图表主色
 * - brandLight → `--td-brand-color-5`，浅色边框 / 次级强调
 * - merge   → `--td-bg-color-page`，页面底色（深色模式下压暗）
 * - border  → `--td-component-border`，卡片与分割线（深色模式下压暗）
 * - accent  → `--app-brand-accent` / `--td-chart-color-2`，点缀与图表第二色
 */
export const FEATURED_PALETTES: readonly PaletteDef[] = [
  {
    id: 'mist-blue',
    name: '雾蓝流金',
    note: '晨雾蓝 × 霜月白 × 落日流金 —— 当前默认',
    brand: '#6F8AB8',
    brandLight: '#A5B3CB',
    merge: '#F2F2EE',
    border: '#AAB7C2',
    accent: '#C1AC7B',
  },
  {
    id: 'deep-sea',
    name: '深海蓝',
    note: '深海蓝 × 雾蓝灰 × 漂木棕 —— 早期版本配色',
    brand: '#51697C',
    brandLight: '#AAB7C2',
    merge: '#F2F2EE',
    border: '#C6D0D8',
    accent: '#B79D88',
  },
  {
    id: 'teal-sunset',
    name: '青碧落日',
    note: '苔原青碧 × 晨光交融 × 落日橘金',
    brand: '#6FB8AA',
    brandLight: '#A5CBC3',
    merge: '#E4EAE1',
    border: '#CFB8AA',
    accent: '#C1AC7B',
  },
  {
    id: 'rose-gold',
    name: '玫紫流金',
    note: '霞紫 × 金玫交融 × 落日流金',
    brand: '#BA6DA7',
    brandLight: '#CCA4C2',
    merge: '#E8EAE1',
    border: '#D0C7A9',
    accent: '#C3B179',
  },
  {
    id: 'violet-honey',
    name: '黛紫蜜橘',
    note: '暮山黛紫 × 胭脂交融 × 暖蜜橘',
    brand: '#A26FB8',
    // ⚠ 需求原稿此处写作 `#BFS5CB`（含非法字符 S，不是合法十六进制）。
    //   按「主色同色相浅色」规律推断为 #BFA5CB（H≈281 与主色 #A26FB8 一致，L 抬高约 12%）。
    //   ★ 该值为**推断值**，拿到原稿修正后只需改这一处，其余全部自动重算。
    brandLight: '#BFA5CB',
    merge: '#EAE1E6',
    border: '#CFB8AA',
    accent: '#C1957B',
  },
  {
    id: 'jade-crimson',
    name: '翠绿绛红',
    note: '翠竹绿 × 朱砂交融 × 绛红渐染',
    brand: '#6FB885',
    brandLight: '#A5CBB0',
    merge: '#E8EAE1',
    border: '#CFAAAB',
    accent: '#C17B7E',
  },
  {
    id: 'glaze-teal',
    name: '釉青',
    note: '湖缥 × 银贝 × 墨青 —— 冷瓷清透',
    brand: '#7BA0A3',
    brandLight: '#A3BEBC',
    merge: '#ECEEEB',
    border: '#CDDDDF',
    accent: '#567076',
  },
  {
    id: 'inkberry',
    name: '墨莓',
    note: '绛莓 × 雪絮 × 夜绀 —— 沉静莓紫',
    brand: '#7D5266',
    brandLight: '#B299AB',
    merge: '#EDEBE9',
    border: '#D3C4CB',
    accent: '#484D61',
  },
  {
    id: 'clear-sky',
    name: '晴空奶黄',
    note: '晴空奶黄 —— #3674B5 × #F5F0CD × #FADA7A',
    brand: '#3674B5',
    brandLight: '#578FCA',
    merge: '#F5F0CD',
    border: '#E4DDB4',
    accent: '#FADA7A',
  },
  {
    id: 'peach-sky',
    name: '蜜桃晴空',
    note: '蜜桃晴空 —— #E78F81 × #FFF5CD × #B7E0FF',
    brand: '#E78F81',
    brandLight: '#FFCFB3',
    merge: '#FFF5CD',
    border: '#F2DDBB',
    accent: '#B7E0FF',
  },
  {
    id: 'cream-celadon',
    name: '奶黄青瓷',
    note: '奶黄青瓷 —— #7EACB5 × #FFF4EA × #C96868',
    brand: '#7EACB5',
    brandLight: '#BFD5DA',
    merge: '#FFF4EA',
    border: '#FADFA1',
    accent: '#C96868',
  },
  {
    id: 'celadon-mist',
    name: '青瓷粉雾',
    note: '青瓷粉雾 —— #789DBC × #FEF9F2 × #A6BBCF',
    brand: '#789DBC',
    brandLight: '#C9E9D2',
    merge: '#FEF9F2',
    border: '#FFE3E3',
    accent: '#A6BBCF',
  },
  {
    id: 'spring-sugar',
    name: '春日糖光',
    note: '春日糖光 —— #A3CCDA × #F8F7BA × #BDE3C3',
    brand: '#A3CCDA',
    brandLight: '#BDE3C3',
    merge: '#F8F7BA',
    border: '#F5D2D2',
    accent: '#BDE3C3',
  },
  {
    id: 'coral-bay',
    name: '珊瑚浅湾',
    note: '珊瑚浅湾 —— #FF6969 × #FFF9DE × #A6D0DD',
    brand: '#FF6969',
    brandLight: '#FFD3B0',
    merge: '#FFF9DE',
    border: '#F3E4BC',
    accent: '#A6D0DD',
  },
  {
    id: 'peacock',
    name: '孔雀蓝',
    note: '孔雀蓝 —— #0F7C90 × #F4F7F8 × #C58A2A',
    brand: '#0F7C90',
    brandLight: '#7BB4BE',
    merge: '#F4F7F8',
    border: '#6C7278',
    accent: '#C58A2A',
  },
  {
    id: 'amber',
    name: '琥珀金',
    note: '琥珀金 —— #C58A2A × #F4F7F8 × #0F7C90',
    brand: '#C58A2A',
    brandLight: '#DDBF87',
    merge: '#F4F7F8',
    border: '#6E5846',
    accent: '#0F7C90',
  },
  {
    id: 'jade',
    name: '翡翠青',
    note: '翡翠青 —— #5E9B84 × #EEF1EB × #CBB28A',
    brand: '#5E9B84',
    brandLight: '#A7C9BE',
    merge: '#EEF1EB',
    border: '#5D6965',
    accent: '#CBB28A',
  },
  {
    id: 'rouge',
    name: '胭脂红',
    note: '胭脂红 —— #A94A63 × #F5F1F1 × #C58A2A',
    brand: '#A94A63',
    brandLight: '#CC94A3',
    merge: '#F5F1F1',
    border: '#82727A',
    accent: '#C58A2A',
  },
  {
    id: 'grape',
    name: '葡萄紫',
    note: '葡萄紫 —— #6F52C9 × #F3EEF7 × #D7B15A',
    brand: '#6F52C9',
    brandLight: '#A896DE',
    merge: '#F3EEF7',
    border: '#8FA9C7',
    accent: '#D7B15A',
  },
  {
    id: 'moon-white',
    name: '月白',
    note: '月白 —— #B8C7D3 × #F4F7F8 × #D8C8A9',
    brand: '#B8C7D3',
    brandLight: '#D6DADF',
    merge: '#F4F7F8',
    border: '#C9CFD5',
    accent: '#D8C8A9',
  },
  {
    id: 'clear-mist',
    name: '霁青',
    note: '霁青 —— #7FA7C7 × #F4F7F8 × #C58A2A',
    brand: '#7FA7C7',
    brandLight: '#B9CDDF',
    merge: '#F4F7F8',
    border: '#97A2AD',
    accent: '#C58A2A',
  },
  {
    id: 'deep-blue-orange',
    name: '深蓝与橙',
    note: '深蓝与橙 —— #112532 × #F4F7F8 × #E0680E',
    brand: '#112532',
    brandLight: '#88A5B7',
    merge: '#F4F7F8',
    border: '#5F7583',
    accent: '#E0680E',
  },
];

// <gallery-data:begin>
/** gallery 77 套（由 scripts/verify/gen_palette_schemes.py 从 docs/design/palette-schemes-77.json 生成，勿手改） */
export const GALLERY_PALETTES: readonly PaletteDef[] = [
  {
    id: 'nat-canglan-naiyou',
    name: '藏蓝奶油',
    note: '藏蓝 × 奶油白 —— 来源 IMG_9126.JPG',
    brand: '#24364A',
    brandLight: '#496D95',
    merge: '#F4E8DA',
    border: '#EBD4BB',
    accent: '#4A3824',
    group: 'fashion',
    source: 'IMG_9126.JPG',
    sourceColors: ['#24364A', '#F4E8DA'],
  }, // #1 源 2 色 · 派生：brandLight、border、accent
  {
    id: 'nat-yunhui-tuo',
    name: '云灰驼',
    note: '#ECECEC × #E6E0D7 × #C7B8A7 —— Today\'s color matching',
    brand: '#C7B8A7',
    brandLight: '#F3EFEC',
    merge: '#ECECEC',
    border: '#D8D8D8',
    accent: '#E6E0D7',
    group: 'fashion',
    source: 'IMG_9127.JPG',
    sourceColors: ['#ECECEC', '#E6E0D7', '#C7B8A7'],
  }, // #2 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-shenhai-wulan',
    name: '深海雾蓝',
    note: '深海灰蓝（主色） × 雾感浅蓝（辅助色） —— 来源 IMG_9128.JPG',
    brand: '#41647F',
    brandLight: '#779CB9',
    merge: '#94BBD2',
    border: '#77A9C6',
    accent: '#7F5C41',
    group: 'fashion',
    source: 'IMG_9128.JPG',
    sourceColors: ['#41647F', '#94BBD2'],
  }, // #3 源 2 色 · 派生：brandLight、border、accent
  {
    id: 'guci-jiutao',
    name: '粉彩九桃天球瓶',
    note: '瓷胎白 × 胭脂红 × 青瓷绿 —— 粉彩九桃天球瓶',
    brand: '#B02A46',
    brandLight: '#DC6E85',
    merge: '#E8E1D6',
    border: '#D9CEBC',
    accent: '#8FA878',
    group: 'porcelain',
    source: 'IMG_9129.JPG',
    sourceColors: ['#E8E1D6', '#B02A46', '#8FA878'],
  }, // #4 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-hulu',
    name: '金地粉彩缠枝莲葫芦瓶',
    note: '瓷胎白 × 青花蓝 × 胚黄 —— 金地粉彩缠枝莲葫芦瓶',
    brand: '#0D3A91',
    brandLight: '#2367EB',
    merge: '#E8E1D6',
    border: '#D9CEBC',
    accent: '#D4B24F',
    group: 'porcelain',
    source: 'IMG_9130.JPG',
    sourceColors: ['#E8E1D6', '#0D3A91', '#D4B24F'],
  }, // #5 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-zhuangxin',
    name: '青花游鱼转心瓶',
    note: '瓷胎白 × 青花蓝 × 云水蓝 —— 青花游鱼转心瓶',
    brand: '#0D3A91',
    brandLight: '#2367EB',
    merge: '#E8E1D6',
    border: '#D9CEBC',
    accent: '#7793C3',
    group: 'porcelain',
    source: 'IMG_9131.JPG',
    sourceColors: ['#E8E1D6', '#0D3A91', '#7793C3'],
  }, // #6 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-zhuangxin-2',
    name: '青花游鱼转心瓶·龙纹',
    note: '瓷胎白 × 龙纹红 × 云水蓝 —— 青花游鱼转心瓶',
    brand: '#B02A1F',
    brandLight: '#E2675D',
    merge: '#E8E1D6',
    border: '#D9CEBC',
    accent: '#6F8EC7',
    group: 'porcelain',
    source: 'IMG_9132.JPG',
    sourceColors: ['#E8E1D6', '#B02A1F', '#6F8EC7'],
  }, // #7 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-wanhua',
    name: '粉彩万花葫芦瓶',
    note: '蔷薇粉 × 雾霭蓝 × 珊瑚橘 —— 粉彩万花葫芦瓶',
    brand: '#81879F',
    brandLight: '#C1C4D0',
    merge: '#EB9075',
    border: '#E67351',
    accent: '#D55B67',
    group: 'porcelain',
    source: 'IMG_9133.JPG',
    sourceColors: ['#D55B67', '#81879F', '#EB9075'],
  }, // #8 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-huaniao',
    name: '青花粉彩花鸟扁瓶',
    note: '瓷胎白 × 青花蓝 × 淡青绿 × 粉彩粉 —— 青花粉彩花鸟扁瓶',
    brand: '#0D3A91',
    brandLight: '#8DA39C',
    merge: '#E8E1D6',
    border: '#D9CEBC',
    accent: '#D7A1B6',
    group: 'porcelain',
    source: 'IMG_9134.JPG',
    sourceColors: ['#E8E1D6', '#0D3A91', '#8DA39C', '#D7A1B6'],
  }, // #9 源 4 色 · 派生：border
  {
    id: 'guci-meiping',
    name: '元青花四爱图梅瓶',
    note: '瓷胎白 × 青花蓝 × 云水蓝 —— 元青花四爱图梅瓶',
    brand: '#0D3A91',
    brandLight: '#2367EB',
    merge: '#E8E1D6',
    border: '#D9CEBC',
    accent: '#7793C3',
    group: 'porcelain',
    source: 'IMG_9135.JPG',
    sourceColors: ['#E8E1D6', '#0D3A91', '#7793C3'],
  }, // #10 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-fushou',
    name: '粉彩镂空福寿双耳瓶',
    note: '瓷胎白 × 釉黄 × 朱砂红 —— 粉彩镂空蟠螭福寿字双耳瓶',
    brand: '#B7372D',
    brandLight: '#DD7E77',
    merge: '#F0EADF',
    border: '#E3D8C3',
    accent: '#D4B13D',
    group: 'porcelain',
    source: 'IMG_9136.JPG',
    sourceColors: ['#F0EADF', '#D4B13D', '#B7372D'],
  }, // #11 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-dizhonghai',
    name: '地中海红蓝',
    note: '赭红 × 海蓝 × 岩白 × 墨黑 —— 地中海红蓝',
    brand: '#1A1A1A',
    brandLight: '#0E4A7B',
    merge: '#F3E8DA',
    border: '#E9D5BB',
    accent: '#C13320',
    group: 'fashion',
    source: 'IMG_9137.PNG',
    sourceColors: ['#C13320', '#0E4A7B', '#F3E8DA', '#1A1A1A'],
  }, // #12 源 4 色 · 派生：border
  {
    id: 'guci-qingluan',
    name: '晴峦叠翠',
    note: '石青 × 松绿 × 岩灰 —— 晴峦叠翠',
    brand: '#4F7D78',
    brandLight: '#88B4AF',
    merge: '#A8AAA1',
    border: '#94978C',
    accent: '#6C8A69',
    group: 'porcelain',
    source: 'IMG_9138.JPG',
    sourceColors: ['#4F7D78', '#6C8A69', '#A8AAA1'],
  }, // #13 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-qianli',
    name: '青绿千里',
    note: '天水碧 × 烟蓝 × 沙汀米 —— 青绿千里·东方山水配色',
    brand: '#77939D',
    brandLight: '#B8C7CC',
    merge: '#D4C5AA',
    border: '#C6B38F',
    accent: '#8BB6B1',
    group: 'porcelain',
    source: 'IMG_9139.JPG',
    sourceColors: ['#8BB6B1', '#77939D', '#D4C5AA'],
  }, // #14 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-yuanshan',
    name: '远山入黛',
    note: '黛蓝 × 苍山青 × 雾白 —— 远山入黛',
    brand: '#40566B',
    brandLight: '#718EAA',
    merge: '#D6D8D4',
    border: '#C2C5BF',
    accent: '#657B74',
    group: 'porcelain',
    source: 'IMG_9140.JPG',
    sourceColors: ['#40566B', '#657B74', '#D6D8D4'],
  }, // #15 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-danxia',
    name: '丹霞赭石',
    note: '丹霞 × 赭石 × 日影金 —— 来源 IMG_9141.JPG',
    brand: '#9B5E45',
    brandLight: '#C99A87',
    merge: '#D1A056',
    border: '#C88E36',
    accent: '#C46B52',
    group: 'fashion',
    source: 'IMG_9141.JPG',
    sourceColors: ['#C46B52', '#9B5E45', '#D1A056'],
  }, // #16 源 3 色 · 派生：brandLight、border
  {
    id: 'pms-sports-outdoor',
    name: '咖啡里昂蓝',
    note: '深牛奶咖啡色 × 巧克力软糖 × 黎明灰 × 里昂蓝 —— SPORTS&OUTDOOR（17-1227 / 19-1432 / 14-4106 / 19-4340）',
    brand: '#492610',
    brandLight: '#175973',
    merge: '#BFC6CE',
    border: '#A8B1BC',
    accent: '#AB8570',
    group: 'fashion',
    source: 'IMG_9142.JPG',
    sourceColors: ['#AB8570', '#492610', '#BFC6CE', '#175973'],
  }, // #17 源 4 色 · 派生：border
  {
    id: 'pms-popular',
    name: '沙灰肉桂',
    note: '沙灰色 × 肉桂卷 × 朗姆酒色 × 水花四溅 —— 流行色（13-1010 / 17-1324 / 19-1321 / 16-4520）',
    brand: '#593833',
    brandLight: '#93755D',
    merge: '#E4C9AE',
    border: '#DAB58F',
    accent: '#6FC4EB',
    group: 'fashion',
    source: 'IMG_9143.JPG',
    sourceColors: ['#E4C9AE', '#93755D', '#593833', '#6FC4EB'],
  }, // #18 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-2',
    name: '灰紫波塞冬',
    note: '浅灰紫 × 蘑菇色 × 小鹿色 × 波塞冬蓝 —— SPORTS&OUTDOOR（13-3803 / 17-1212 / 16-1510 / 19-4033）',
    brand: '#062D4C',
    brandLight: '#8F8176',
    merge: '#D5CDCA',
    border: '#C3B8B3',
    accent: '#AC8F8B',
    group: 'fashion',
    source: 'IMG_9144.JPG',
    sourceColors: ['#D5CDCA', '#8F8176', '#AC8F8B', '#062D4C'],
  }, // #19 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-3',
    name: '早霜阿拉斯加',
    note: '早霜 × 爱尔兰奶油 × 阿拉斯加蓝 × 巧克力毛毛雨 —— SPORTS&OUTDOOR（12-4001 / 14-1208 / 15-4225 / 19-1410）',
    brand: '#35140F',
    brandLight: '#79AED8',
    merge: '#DCD9D2',
    border: '#CAC6BB',
    accent: '#C1B198',
    group: 'fashion',
    source: 'IMG_9145.JPG',
    sourceColors: ['#DCD9D2', '#C1B198', '#79AED8', '#35140F'],
  }, // #20 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-4',
    name: '榛子翠蓝',
    note: '古裙衬 × 榛子 × 有机灰 × 翠蓝眼蛱蝶 —— SPORTS&OUTDOOR（12-4401 / 14-1315 / 18-0407 / 19-4040）',
    brand: '#1C3860',
    brandLight: '#6A615C',
    merge: '#DCDCDC',
    border: '#C8C8C8',
    accent: '#CDAA8C',
    group: 'fashion',
    source: 'IMG_9146.JPG',
    sourceColors: ['#DCDCDC', '#CDAA8C', '#6A615C', '#1C3860'],
  }, // #21 源 4 色 · 派生：border
  {
    id: 'pms-shuangzi',
    name: '霜紫宇宙紫',
    note: '霜紫色 × 宇宙紫 × 古裙衬 × 摩卡酥皮 —— 来源 IMG_9147.JPG（15-3507 / 19-3714 / 12-4401 / 16-1211）',
    brand: '#5A425C',
    brandLight: '#9E887B',
    merge: '#D7DBDC',
    border: '#C1C7C9',
    accent: '#B2A4B5',
    group: 'fashion',
    source: 'IMG_9147.JPG',
    sourceColors: ['#B2A4B5', '#5A425C', '#D7DBDC', '#9E887B'],
  }, // #22 源 4 色 · 派生：border
  {
    id: 'pms-chengbao',
    name: '城堡波特酒',
    note: '城堡灰 × 陈年波特酒色 × 梅酒色 × 阿拉斯加蓝 —— 来源 IMG_9148.JPG（15-4503 / 19-1725 / 18-1411 / 15-4225）',
    brand: '#582933',
    brandLight: '#6B484E',
    merge: '#BBB2A9',
    border: '#A99E92',
    accent: '#73A5C8',
    group: 'fashion',
    source: 'IMG_9148.JPG',
    sourceColors: ['#BBB2A9', '#582933', '#6B484E', '#73A5C8'],
  }, // #23 源 4 色 · 派生：border
  {
    id: 'pms-xinchao',
    name: '新潮空间',
    note: '巧克力软糖 × 饼干色 × 海胆色 × 野鸭蓝 —— 新潮空间（19-1432 / 16-1336 / 13-1106 / 19-4318）',
    brand: '#56352E',
    brandLight: '#3C5F73',
    merge: '#DFCCBE',
    border: '#D2B7A3',
    accent: '#B0835A',
    group: 'fashion',
    source: 'IMG_9149.JPG',
    sourceColors: ['#56352E', '#B0835A', '#DFCCBE', '#3C5F73'],
  }, // #24 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-5',
    name: '海洋洞穴',
    note: '柔和蓝 × 海洋洞穴 × 胡荽色 × 珊瑚沙色 —— SPORTS&OUTDOOR（14-4209 / 19-3937 / 17-1113 / 14-1224）',
    brand: '#1C203D',
    brandLight: '#908571',
    merge: '#F2B08E',
    border: '#EE9669',
    accent: '#A7B9CD',
    group: 'fashion',
    source: 'IMG_9150.JPG',
    sourceColors: ['#A7B9CD', '#1C203D', '#908571', '#F2B08E'],
  }, // #25 源 4 色 · 派生：border
  {
    id: 'nat-img9151',
    name: '色卡 9151',
    note: '#A45668 × #D59BA8 × #C7C1B5 × #E7DDD3 —— 来源 IMG_9151.JPG',
    brand: '#A45668',
    brandLight: '#D59BA8',
    merge: '#E7DDD3',
    border: '#D9C9B9',
    accent: '#C7C1B5',
    group: 'fashion',
    source: 'IMG_9151.JPG',
    sourceColors: ['#A45668', '#D59BA8', '#C7C1B5', '#E7DDD3'],
  }, // #26 源 4 色 · 派生：border
  {
    id: 'nat-img9152',
    name: '色卡 9152',
    note: '#8C2F2D × #B76F6A × #8E8075 × #B7C5B7 —— 来源 IMG_9152.JPG',
    brand: '#8C2F2D',
    brandLight: '#8E8075',
    merge: '#B7C5B7',
    border: '#A0B3A0',
    accent: '#B76F6A',
    group: 'fashion',
    source: 'IMG_9152.JPG',
    sourceColors: ['#8C2F2D', '#B76F6A', '#8E8075', '#B7C5B7'],
  }, // #27 源 4 色 · 派生：border
  {
    id: 'nat-img9153',
    name: '色卡 9153',
    note: '#4F6072 × #B56A58 × #D9B07C × #8DB8A7 —— 来源 IMG_9153.JPG',
    brand: '#4F6072',
    brandLight: '#B56A58',
    merge: '#D9B07C',
    border: '#D09D5C',
    accent: '#8DB8A7',
    group: 'fashion',
    source: 'IMG_9153.JPG',
    sourceColors: ['#4F6072', '#B56A58', '#D9B07C', '#8DB8A7'],
  }, // #28 源 4 色 · 派生：border
  {
    id: 'nat-img9154',
    name: '色卡 9154',
    note: '#607E95 × #A8C3D6 × #B8AEA6 × #E2D0BC —— 来源 IMG_9154.JPG',
    brand: '#607E95',
    brandLight: '#B8AEA6',
    merge: '#E2D0BC',
    border: '#D6BCA0',
    accent: '#A8C3D6',
    group: 'fashion',
    source: 'IMG_9154.JPG',
    sourceColors: ['#607E95', '#A8C3D6', '#B8AEA6', '#E2D0BC'],
  }, // #29 源 4 色 · 派生：border
  {
    id: 'nat-img9155',
    name: '色卡 9155',
    note: '#5B6466 × #8C9B9F × #C6C9C0 × #DFE2DA —— 来源 IMG_9155.JPG',
    brand: '#5B6466',
    brandLight: '#8C9B9F',
    merge: '#DFE2DA',
    border: '#CBD0C3',
    accent: '#C6C9C0',
    group: 'fashion',
    source: 'IMG_9155.JPG',
    sourceColors: ['#5B6466', '#8C9B9F', '#C6C9C0', '#DFE2DA'],
  }, // #30 源 4 色 · 派生：border
  {
    id: 'nat-img9156',
    name: '色卡 9156',
    note: '#B7C4CF × #D1D8DC × #E8E9E6 × #F1F2EE —— 来源 IMG_9156.JPG',
    brand: '#B7C4CF',
    brandLight: '#D1D8DC',
    merge: '#F1F2EE',
    border: '#DEE0D7',
    accent: '#E8E9E6',
    group: 'fashion',
    source: 'IMG_9156.JPG',
    sourceColors: ['#B7C4CF', '#D1D8DC', '#E8E9E6', '#F1F2EE'],
  }, // #31 源 4 色 · 派生：border
  {
    id: 'nat-img9157',
    name: '色卡 9157',
    note: '#4A6460 × #7A9387 × #BFD0B8 × #DDE6D6 —— 来源 IMG_9157.JPG',
    brand: '#4A6460',
    brandLight: '#7A9387',
    merge: '#DDE6D6',
    border: '#C8D7BD',
    accent: '#BFD0B8',
    group: 'fashion',
    source: 'IMG_9157.JPG',
    sourceColors: ['#4A6460', '#7A9387', '#BFD0B8', '#DDE6D6'],
  }, // #32 源 4 色 · 派生：border
  {
    id: 'nat-img9158',
    name: '色卡 9158',
    note: '#2E6A67 × #5E8C87 × #A9C8BE × #D7E6DB —— 来源 IMG_9158.JPG',
    brand: '#2E6A67',
    brandLight: '#5E8C87',
    merge: '#D7E6DB',
    border: '#BED6C4',
    accent: '#A9C8BE',
    group: 'fashion',
    source: 'IMG_9158.JPG',
    sourceColors: ['#2E6A67', '#5E8C87', '#A9C8BE', '#D7E6DB'],
  }, // #33 源 4 色 · 派生：border
  {
    id: 'nat-img9159',
    name: '色卡 9159',
    note: '#7E6554 × #A78A73 × #D4B79D × #E9D6BE —— 来源 IMG_9159.JPG',
    brand: '#7E6554',
    brandLight: '#A78A73',
    merge: '#E9D6BE',
    border: '#DFC3A0',
    accent: '#D4B79D',
    group: 'fashion',
    source: 'IMG_9159.JPG',
    sourceColors: ['#7E6554', '#A78A73', '#D4B79D', '#E9D6BE'],
  }, // #34 源 4 色 · 派生：border
  {
    id: 'guci-youlibai',
    name: '釉里白',
    note: '釉里白 × 苏麻青 × 淡烟灰 —— 来源 IMG_9160.JPG',
    brand: '#315F86',
    brandLight: '#6298C5',
    merge: '#F2EEE5',
    border: '#E4DCCA',
    accent: '#B7BEC3',
    group: 'porcelain',
    source: 'IMG_9160.JPG',
    sourceColors: ['#F2EEE5', '#315F86', '#B7BEC3'],
  }, // #35 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-ciyun',
    name: '青花瓷韵',
    note: '月白 × 豆青 × 花叶青 —— 青花瓷韵 · 东方蓝白配色 · 缠枝含露',
    brand: '#5F817B',
    brandLight: '#9BB5B1',
    merge: '#EEF1ED',
    border: '#D8DFD6',
    accent: '#9CB4AE',
    group: 'porcelain',
    source: 'IMG_9161.JPG',
    sourceColors: ['#EEF1ED', '#9CB4AE', '#5F817B'],
  }, // #36 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-haishui',
    name: '海水江崖',
    note: '霁蓝 × 浪花白 × 天水碧 —— 海水江崖',
    brand: '#244A73',
    brandLight: '#4381C4',
    merge: '#F5F2EA',
    border: '#E8E1CE',
    accent: '#9CC7CF',
    group: 'porcelain',
    source: 'IMG_9162.JPG',
    sourceColors: ['#244A73', '#F5F2EA', '#9CC7CF'],
  }, // #37 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-yingyue',
    name: '夜瓷映月',
    note: '墨青 × 冷月白 × 银灰 —— 夜瓷映月',
    brand: '#263D46',
    brandLight: '#4D7C8F',
    merge: '#E7E3D8',
    border: '#D7D1BF',
    accent: '#B8BDC2',
    group: 'porcelain',
    source: 'IMG_9163.JPG',
    sourceColors: ['#263D46', '#E7E3D8', '#B8BDC2'],
  }, // #38 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-wulan-qing',
    name: '浅雾蓝青',
    note: '浅雾蓝 × 深灰青 × 深青 —— Today\'s color matching',
    brand: '#2F787D',
    brandLight: '#5BBBC1',
    merge: '#D8E6EF',
    border: '#BBD3E3',
    accent: '#4F5F65',
    group: 'fashion',
    source: 'IMG_9164.JPG',
    sourceColors: ['#D8E6EF', '#4F5F65', '#2F787D'],
  }, // #39 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-img9165',
    name: '色卡 9165',
    note: '#800000 × #982B1C × #DAD4B5 —— 来源 IMG_9165.JPG',
    brand: '#800000',
    brandLight: '#F00000',
    merge: '#DAD4B5',
    border: '#CCC49A',
    accent: '#982B1C',
    group: 'fashion',
    source: 'IMG_9165.JPG',
    sourceColors: ['#800000', '#982B1C', '#DAD4B5'],
  }, // #40 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-img9166',
    name: '色卡 9166',
    note: '#FAF2DA × #8E9775 × #4A503D —— 来源 IMG_9166.JPG',
    brand: '#4A503D',
    brandLight: '#85906E',
    merge: '#FAF2DA',
    border: '#F5E5B6',
    accent: '#8E9775',
    group: 'fashion',
    source: 'IMG_9166.JPG',
    sourceColors: ['#FAF2DA', '#8E9775', '#4A503D'],
  }, // #41 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-img9167',
    name: '色卡 9167',
    note: '#97BFB4 × #F5EEDC × #DD4A48 —— 来源 IMG_9167.JPG',
    brand: '#DD4A48',
    brandLight: '#EFA8A7',
    merge: '#F5EEDC',
    border: '#ECDFBC',
    accent: '#97BFB4',
    group: 'fashion',
    source: 'IMG_9167.JPG',
    sourceColors: ['#97BFB4', '#F5EEDC', '#DD4A48'],
  }, // #42 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-img9168',
    name: '色卡 9168',
    note: '#A20A0A × #FFA36C × #F6EEC9 —— 来源 IMG_9168.JPG',
    brand: '#A20A0A',
    brandLight: '#F22A2A',
    merge: '#F6EEC9',
    border: '#F0E3A6',
    accent: '#FFA36C',
    group: 'fashion',
    source: 'IMG_9168.JPG',
    sourceColors: ['#A20A0A', '#FFA36C', '#F6EEC9'],
  }, // #43 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-img9169',
    name: '色卡 9169',
    note: '#AA3A3A × #A4B787 × #F4F4F4 —— 来源 IMG_9169.JPG',
    brand: '#AA3A3A',
    brandLight: '#D48080',
    merge: '#F4F4F4',
    border: '#E0E0E0',
    accent: '#A4B787',
    group: 'fashion',
    source: 'IMG_9169.JPG',
    sourceColors: ['#AA3A3A', '#A4B787', '#F4F4F4'],
  }, // #44 源 3 色 · 派生：brandLight、border
  {
    id: 'nat-img9170',
    name: '色卡 9170',
    note: '#FEF9E1 × #E5D0AC × #A31D1D —— 来源 IMG_9170.JPG',
    brand: '#A31D1D',
    brandLight: '#E05050',
    merge: '#FEF9E1',
    border: '#FDF1BA',
    accent: '#E5D0AC',
    group: 'fashion',
    source: 'IMG_9170.JPG',
    sourceColors: ['#FEF9E1', '#E5D0AC', '#A31D1D'],
  }, // #45 源 3 色 · 派生：brandLight、border
  {
    id: 'pms-sports-outdoor-6',
    name: '羚羊皮翠蓝',
    note: '熟瓜色 × 羚羊皮色 × 露白色 × 翠蓝眼蛱蝶 —— SPORTS&OUTDOOR（16-1260 / 15-1145 / 12-1108 / 19-4040）',
    brand: '#1C3961',
    brandLight: '#CB5E33',
    merge: '#EFDECE',
    border: '#E5C9AF',
    accent: '#F4B06F',
    group: 'fashion',
    source: 'IMG_9171.JPG',
    sourceColors: ['#CB5E33', '#F4B06F', '#EFDECE', '#1C3961'],
  }, // #46 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-7',
    name: '橡金橘探戈',
    note: '橡金色 × 咖哩色 × 浓橘探戈色 × 生皮色 —— SPORTS&OUTDOOR（17-1046 / 16-0928 / 17-1643 / 18-1137）',
    brand: '#8C5F4C',
    brandLight: '#B36D28',
    merge: '#BFA475',
    border: '#B29259',
    accent: '#E94C2D',
    group: 'fashion',
    source: 'IMG_9172.JPG',
    sourceColors: ['#B36D28', '#BFA475', '#E94C2D', '#8C5F4C'],
  }, // #47 源 4 色 · 派生：border
  {
    id: 'pms-shanye',
    name: '到山野间',
    note: '小鹿色 × 韭菜绿 × 庄重 × 狂风 —— 到山野间（16-1510 / 15-0628 / 19-0914 / 17-3912）',
    brand: '#372E2F',
    brandLight: '#5E6883',
    merge: '#BDB17F',
    border: '#AFA164',
    accent: '#B48F87',
    group: 'fashion',
    source: 'IMG_9173.JPG',
    sourceColors: ['#B48F87', '#BDB17F', '#372E2F', '#5E6883'],
  }, // #48 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-8',
    name: '方钠石金猕猴',
    note: '方钠石蓝 × 红棕色 × 鲜紫色 × 金猕猴桃 —— SPORTS&OUTDOOR（19-3953 / 18-1616 / 18-3840 / 13-0644）',
    brand: '#143062',
    brandLight: '#8E565F',
    merge: '#EFDA3F',
    border: '#ECD319',
    accent: '#685DA1',
    group: 'fashion',
    source: 'IMG_9174.JPG',
    sourceColors: ['#143062', '#8E565F', '#685DA1', '#EFDA3F'],
  }, // #49 源 4 色 · 派生：border
  {
    id: 'pms-migua',
    name: '蜜瓜色',
    note: '蜜瓜色 —— 来源 IMG_9175.JPG（13-0814）',
    brand: '#DDDDDD',
    brandLight: '#FFFFFF',
    merge: '#F2F2F2',
    border: '#D1D1D1',
    accent: '#DDDDDD',
    group: 'fashion',
    source: 'IMG_9175.JPG',
    sourceColors: ['#DDDDDD'],
  }, // #50 源 1 色 · 派生：brandLight、merge、border、accent
  {
    id: 'pms-sports-outdoor-9',
    name: '瓦蓝三色堇',
    note: '瓦蓝 × 防风草色 × 粉色饮料 × 三色堇色 —— SPORTS&OUTDOOR（18-4735 / 14-0925 / 14-2216 / 19-3542）',
    brand: '#197F8B',
    brandLight: '#5E3C78',
    merge: '#EE94B0',
    border: '#E87196',
    accent: '#D7C094',
    group: 'fashion',
    source: 'IMG_9176.JPG',
    sourceColors: ['#197F8B', '#D7C094', '#EE94B0', '#5E3C78'],
  }, // #51 源 4 色 · 派生：border
  {
    id: 'pms-shenshuilan',
    name: '深水蓝',
    note: '深水蓝 —— 来源 IMG_9177.JPG（18-4032）',
    brand: '#DDDDDD',
    brandLight: '#FFFFFF',
    merge: '#F2F2F2',
    border: '#D1D1D1',
    accent: '#DDDDDD',
    group: 'fashion',
    source: 'IMG_9177.JPG',
    sourceColors: ['#DDDDDD'],
  }, // #52 源 1 色 · 派生：brandLight、merge、border、accent
  {
    id: 'pms-yueliang',
    name: '月亮蓝',
    note: '月亮蓝 × 神秘蓝 × 番茄酱色 × 深橄榄色 —— 来源 IMG_9178.JPG（17-4328 / 11-4303 / 18-1661 / 19-0516）',
    brand: '#594D33',
    brandLight: '#3890A6',
    merge: '#E6E5E0',
    border: '#D4D2C9',
    accent: '#CB394C',
    group: 'fashion',
    source: 'IMG_9178.JPG',
    sourceColors: ['#3890A6', '#E6E5E0', '#CB394C', '#594D33'],
  }, // #53 源 4 色 · 派生：border
  {
    id: 'pms-miaomiao',
    name: '妙妙王国',
    note: '亮紫色 × 浅湖蓝 × 雨衣 × 明紫色 —— 妙妙王国（18-3025 / 14-4810 / 13-0749 / 18-3820）',
    brand: '#975891',
    brandLight: '#656692',
    merge: '#A9C8CA',
    border: '#90B8BA',
    accent: '#EEC933',
    group: 'fashion',
    source: 'IMG_9179.JPG',
    sourceColors: ['#975891', '#A9C8CA', '#EEC933', '#656692'],
  }, // #54 源 4 色 · 派生：border
  {
    id: 'pms-kaoguo',
    name: '烤果馅饼',
    note: '烤果馅饼 —— SPORTS&OUTDOOR（13-0716）',
    brand: '#DDDDDD',
    brandLight: '#FFFFFF',
    merge: '#F2F2F2',
    border: '#D1D1D1',
    accent: '#DDDDDD',
    group: 'fashion',
    source: 'IMG_9180.JPG',
    sourceColors: ['#DDDDDD'],
  }, // #55 源 1 色 · 派生：brandLight、merge、border、accent
  {
    id: 'pms-xingrenlao',
    name: '杏仁酪',
    note: '杏仁酪色 × 琥珀褐 × 板岩蓝 × 烟松绿 —— 来源 IMG_9181.JPG（13-1017 / 17-1147 / 16-4408 / 18-5718）',
    brand: '#3E685C',
    brandLight: '#A3664A',
    merge: '#E9BB9A',
    border: '#E2A478',
    accent: '#889DA2',
    group: 'fashion',
    source: 'IMG_9181.JPG',
    sourceColors: ['#E9BB9A', '#A3664A', '#889DA2', '#3E685C'],
  }, // #56 源 4 色 · 派生：border
  {
    id: 'pms-heimei',
    name: '黑莓果酱',
    note: '黑莓果酱 × 灰泥色 × 稻草色 × 冬花 —— 来源 IMG_9182.JPG（19-3949 / 16-1412 / 13-0922 / 19-2620）',
    brand: '#2C0A23',
    brandLight: '#2B2855',
    merge: '#E1CC93',
    border: '#D8BD73',
    accent: '#A38D82',
    group: 'fashion',
    source: 'IMG_9182.JPG',
    sourceColors: ['#2B2855', '#A38D82', '#E1CC93', '#2C0A23'],
  }, // #57 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-10',
    name: '褐菇海洋洞穴',
    note: '褐菇 × 咖啡褐色 × 亮白色 × 海洋洞穴 —— SPORTS&OUTDOOR（18-1032 / 19-0712 / 11-0700 / 19-3937）',
    brand: '#382618',
    brandLight: '#142747',
    merge: '#F1F4FB',
    border: '#D1DBF2',
    accent: '#92745C',
    group: 'fashion',
    source: 'IMG_9183.JPG',
    sourceColors: ['#92745C', '#382618', '#F1F4FB', '#142747'],
  }, // #58 源 4 色 · 派生：border
  {
    id: 'pms-hejin',
    name: '时尚合金',
    note: '糙米 × 羊皮色 × 松木色 × 灌浆 —— 时尚·合金（13-1105 / 14-1122 / 17-1525 / 12-4102）',
    brand: '#A66D5A',
    brandLight: '#C5BAA6',
    merge: '#D1D4DB',
    border: '#BABFC9',
    accent: '#DBB994',
    group: 'fashion',
    source: 'IMG_9184.JPG',
    sourceColors: ['#C5BAA6', '#DBB994', '#A66D5A', '#D1D4DB'],
  }, // #59 源 4 色 · 派生：border
  {
    id: 'pms-shanchuang',
    name: '山创空间',
    note: '玉米粒 × 蛋黄色 × 帆船蓝 × 月岩色 —— 山创空间（12-0723 / 14-0846 / 18-4039 / 14-4201）',
    brand: '#4978AE',
    brandLight: '#D6A850',
    merge: '#C5C5C5',
    border: '#B1B1B1',
    accent: '#DCD785',
    group: 'fashion',
    source: 'IMG_9185.JPG',
    sourceColors: ['#DCD785', '#D6A850', '#4978AE', '#C5C5C5'],
  }, // #60 源 4 色 · 派生：border
  {
    id: 'pms-sports-outdoor-11',
    name: '云米山鹑',
    note: '云米色 × 土砂色 × 山鹑色 × 鲍鱼壳色 —— SPORTS&OUTDOOR（12-0804 / 16-1326 / 18-1124 / 17-4017）',
    brand: '#71523D',
    brandLight: '#7F848A',
    merge: '#E8DFC0',
    border: '#DDD0A2',
    accent: '#B99A6E',
    group: 'fashion',
    source: 'IMG_9186.JPG',
    sourceColors: ['#E8DFC0', '#B99A6E', '#71523D', '#7F848A'],
  }, // #61 源 4 色 · 派生：border
  {
    id: 'pms-dama',
    name: '大麻色',
    note: '大麻色 × 完美梨 × 花旗松 × 冰露色 —— 来源 IMG_9187.JPG（14-0721 / 16-0531 / 19-0220 / 13-0513）',
    brand: '#223B1D',
    brandLight: '#8F8639',
    merge: '#DED0B5',
    border: '#D1BE99',
    accent: '#BFAE80',
    group: 'fashion',
    source: 'IMG_9187.JPG',
    sourceColors: ['#BFAE80', '#8F8639', '#223B1D', '#DED0B5'],
  }, // #62 源 4 色 · 派生：border
  {
    id: 'pms-shanxi',
    name: '山系色会',
    note: '树皮色 —— 山系色会（16-1506）',
    brand: '#DDDDDD',
    brandLight: '#FFFFFF',
    merge: '#F2F2F2',
    border: '#D1D1D1',
    accent: '#DDDDDD',
    group: 'fashion',
    source: 'IMG_9188.JPG',
    sourceColors: ['#DDDDDD'],
  }, // #63 源 1 色 · 派生：brandLight、merge、border、accent
  {
    id: 'nat-persimmon',
    name: '柿子色',
    note: '柿子色（译） × 杏色（译） × 暖亚麻色（译） × 鼠尾草橄榄绿（译） —— PERSIMMON',
    brand: '#8A9272',
    brandLight: '#D96B37',
    merge: '#F2DFC5',
    border: '#EBCDA4',
    accent: '#ED9D65',
    group: 'fashion',
    source: 'IMG_9189.PNG',
    sourceColors: ['#D96B37', '#ED9D65', '#F2DFC5', '#8A9272'],
  }, // #64 源 4 色 · 派生：border
  {
    id: 'nat-crested-ibis',
    name: '朱鹮',
    note: 'FEATHER ROSE × SHELL PINK × MILK IVORY × REED MIST —— CRESTED IBIS',
    brand: '#A6B5AF',
    brandLight: '#D9A5A4',
    merge: '#F2E8DA',
    border: '#E7D5BC',
    accent: '#EACAC0',
    group: 'fashion',
    source: 'IMG_9190.PNG',
    sourceColors: ['#D9A5A4', '#EACAC0', '#F2E8DA', '#A6B5AF'],
  }, // #65 源 4 色 · 派生：border
  {
    id: 'nat-bee-eater',
    name: '蓝喉蜂虎',
    note: 'LAGOON × SEA MIST × LEAF LIME × CHESTNUT —— BLUE THROATED BEE EATER',
    brand: '#3F838B',
    brandLight: '#B57B50',
    merge: '#77B7B4',
    border: '#5CA9A5',
    accent: '#8DAD6E',
    group: 'fashion',
    source: 'IMG_9191.PNG',
    sourceColors: ['#3F838B', '#77B7B4', '#8DAD6E', '#B57B50'],
  }, // #66 源 4 色 · 派生：border
  {
    id: 'nat-osmanthus',
    name: '桂花',
    note: 'NECTAR GOLD × CREAM PETAL × SOFT SAGE × LEAF GREEN —— OSMANTHUS',
    brand: '#697D58',
    brandLight: '#E8B957',
    merge: '#F4DFC0',
    border: '#EECD9D',
    accent: '#A9B491',
    group: 'fashion',
    source: 'IMG_9192.PNG',
    sourceColors: ['#E8B957', '#F4DFC0', '#A9B491', '#697D58'],
  }, // #67 源 4 色 · 派生：border
  {
    id: 'nat-muhly-grass',
    name: '芒草',
    note: '暮色玫瑰（译） × 尘粉腮红（译） × 花瓣云（译） × 燕麦奶（译） —— MUHLY GRASS',
    brand: '#A7697D',
    brandLight: '#D2A0AD',
    merge: '#EAE5D9',
    border: '#DBD3BF',
    accent: '#E8CDD2',
    group: 'fashion',
    source: 'IMG_9193.PNG',
    sourceColors: ['#A7697D', '#D2A0AD', '#E8CDD2', '#EAE5D9'],
  }, // #68 源 4 色 · 派生：border
  {
    id: 'nat-huxiu',
    name: '狐锈红',
    note: '狐锈红（译） × 柔铜色（译） × 毛皮奶油色（译） × 草灰褐色（译） —— 来源 IMG_9194.PNG',
    brand: '#B95E3F',
    brandLight: '#A5A18C',
    merge: '#F0D9B9',
    border: '#E9C797',
    accent: '#D59670',
    group: 'fashion',
    source: 'IMG_9194.PNG',
    sourceColors: ['#B95E3F', '#D59670', '#F0D9B9', '#A5A18C'],
  }, // #69 源 4 色 · 派生：border
  {
    id: 'nat-oak',
    name: '橡木',
    note: '橡果色（译） × 烤蜂蜜色（译） × 燕麦米色（译） × 浅苔藓色（译） —— OAK',
    brand: '#916449',
    brandLight: '#BD9669',
    merge: '#DEC7A3',
    border: '#D3B585',
    accent: '#B2B39A',
    group: 'fashion',
    source: 'IMG_9195.PNG',
    sourceColors: ['#916449', '#BD9669', '#DEC7A3', '#B2B39A'],
  }, // #70 源 4 色 · 派生：border
  {
    id: 'nat-maple',
    name: '枫',
    note: 'MAPLE RED × BURNT PEACH × AMBER LIGHT × MIST LINEN —— MAPLE',
    brand: '#A34D49',
    brandLight: '#CE7956',
    merge: '#DCD5C7',
    border: '#CCC2AE',
    accent: '#E6B98A',
    group: 'fashion',
    source: 'IMG_9196.PNG',
    sourceColors: ['#A34D49', '#CE7956', '#E6B98A', '#DCD5C7'],
  }, // #71 源 4 色 · 派生：border
  {
    id: 'nat-chanterelle',
    name: '鸡油菌',
    note: '鸡油菌色（译） × 黄油金色（译） × 暖燕麦色（译） × 地衣色（译） —— CHANTERELLE',
    brand: '#929B69',
    brandLight: '#D19A40',
    merge: '#ECDDC1',
    border: '#E2CCA2',
    accent: '#E8BF6A',
    group: 'fashion',
    source: 'IMG_9197.PNG',
    sourceColors: ['#D19A40', '#E8BF6A', '#ECDDC1', '#929B69'],
  }, // #72 源 4 色 · 派生：border
  {
    id: 'guci-fenqiang',
    name: '粉墙桂蕊',
    note: '粉墙白 × 桂蕊金 × 苔痕青 —— 来源 IMG_9198.JPG',
    brand: '#70806B',
    brandLight: '#AAB5A6',
    merge: '#F3EFE4',
    border: '#E6DEC8',
    accent: '#D8A847',
    group: 'porcelain',
    source: 'IMG_9198.JPG',
    sourceColors: ['#F3EFE4', '#D8A847', '#70806B'],
  }, // #73 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-chishui',
    name: '池水太湖',
    note: '池水青 × 太湖灰 × 枫叶朱 —— 来源 IMG_9199.JPG',
    brand: '#B95C43',
    brandLight: '#D8A294',
    merge: '#D6D0C3',
    border: '#C5BDAB',
    accent: '#9DBBB5',
    group: 'porcelain',
    source: 'IMG_9199.JPG',
    sourceColors: ['#9DBBB5', '#D6D0C3', '#B95C43'],
  }, // #74 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-xuanzhi',
    name: '宣纸晚霞',
    note: '宣纸白 × 晚霞杏 × 竹叶青 —— 来源 IMG_9200.JPG',
    brand: '#697553',
    brandLight: '#A1AD8B',
    merge: '#F2E7D3',
    border: '#E9D6B4',
    accent: '#E5A360',
    group: 'porcelain',
    source: 'IMG_9200.JPG',
    sourceColors: ['#F2E7D3', '#E5A360', '#697553'],
  }, // #75 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-yuedong',
    name: '月洞桂香',
    note: '月洞白 × 桂香金 × 夜池青 —— 来源 IMG_9201.JPG',
    brand: '#486E73',
    brandLight: '#7DA8AE',
    merge: '#E8E2D6',
    border: '#D9D0BC',
    accent: '#D4A64C',
    group: 'porcelain',
    source: 'IMG_9201.JPG',
    sourceColors: ['#E8E2D6', '#D4A64C', '#486E73'],
  }, // #76 源 3 色 · 派生：brandLight、border
  {
    id: 'guci-gongming',
    name: '暮雪宫门',
    note: '暮雪灰 × 宫门朱 × 宫灯金 —— 来源 IMG_9202.JPG',
    brand: '#8C3D34',
    brandLight: '#C77369',
    merge: '#D8DADF',
    border: '#C2C5CD',
    accent: '#D7A35A',
    group: 'porcelain',
    source: 'IMG_9202.JPG',
    sourceColors: ['#D8DADF', '#8C3D34', '#D7A35A'],
  }, // #77 源 3 色 · 派生：brandLight、border
];
// <gallery-data:end>

/** 全部方案：精选 22 + gallery 77 = 99 套（数组顺序即设置页展示顺序） */
export const PALETTES: readonly PaletteDef[] = [...FEATURED_PALETTES, ...GALLERY_PALETTES];

/** 分组元信息（顺序即设置页分区顺序） */
export const PALETTE_GROUPS: readonly { id: PaletteGroup; name: string; note: string }[] = [
  { id: 'featured', name: '云枢精选', note: '底座自带 22 套，色值来自需求方色表' },
  { id: 'porcelain', name: '古典釉色', note: '瓷器 / 釉色提取 20 套：青花、粉彩、窑变' },
  { id: 'fashion', name: '时尚配色', note: 'PANTONE 与自然灵感 57 套：四色趋势、鸟兽草木' },
];

/** 取方案所属分组（历史数据没有 group 字段的按 featured 处理） */
export function groupOf(palette: PaletteDef): PaletteGroup {
  return palette.group ?? 'featured';
}

/** 默认方案（首次访问） */
export const DEFAULT_PALETTE_ID: PaletteId = 'mist-blue';

/** 是否为合法方案 id（localStorage 脏数据兜底用） */
export function isPaletteId(value: unknown): value is PaletteId {
  return typeof value === 'string' && PALETTES.some((item) => item.id === value);
}

/** 按 id 取方案，id 非法时回退默认方案 */
export function getPalette(id: string | null | undefined): PaletteDef {
  return PALETTES.find((item) => item.id === id) ?? PALETTES[0];
}

/* ============================================================================
 * 一、颜色基础运算（纯函数，无依赖，脚本与浏览器共用）
 * ========================================================================== */

/** RGB 三元组，分量 0~255 */
export interface Rgb {
  r: number;
  g: number;
  b: number;
}

/** HSL 三元组，h 0~360，s/l 0~100 */
export interface Hsl {
  h: number;
  s: number;
  l: number;
}

function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value;
}

/** 十六进制色值 → RGB；非法色值直接抛错（色表里出现过 `#BFS5CB` 这种脏数据） */
export function parseHex(hex: string): Rgb {
  const raw = hex.trim().replace(/^#/, '');
  const full =
    raw.length === 3
      ? raw
          .split('')
          .map((ch) => ch + ch)
          .join('')
      : raw;
  if (!/^[0-9a-fA-F]{6}$/.test(full)) {
    throw new Error(`非法十六进制色值：${hex}`);
  }
  return {
    r: parseInt(full.slice(0, 2), 16),
    g: parseInt(full.slice(2, 4), 16),
    b: parseInt(full.slice(4, 6), 16),
  };
}

/** RGB → `#rrggbb`（小写，便于断言比较） */
export function toHex(rgb: Rgb): string {
  const part = (value: number): string =>
    Math.round(clamp(value, 0, 255))
      .toString(16)
      .padStart(2, '0');
  return `#${part(rgb.r)}${part(rgb.g)}${part(rgb.b)}`;
}

/** RGB → HSL */
export function rgbToHsl(rgb: Rgb): Hsl {
  const r = rgb.r / 255;
  const g = rgb.g / 255;
  const b = rgb.b / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) {
    return { h: 0, s: 0, l: l * 100 };
  }
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === r) {
    h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
  } else if (max === g) {
    h = ((b - r) / d + 2) / 6;
  } else {
    h = ((r - g) / d + 4) / 6;
  }
  return { h: h * 360, s: s * 100, l: l * 100 };
}

/** HSL → RGB */
export function hslToRgb(hsl: Hsl): Rgb {
  const h = ((hsl.h % 360) + 360) % 360 / 360;
  const s = clamp(hsl.s, 0, 100) / 100;
  const l = clamp(hsl.l, 0, 100) / 100;
  if (s === 0) {
    const v = l * 255;
    return { r: v, g: v, b: v };
  }
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const hue = (t: number): number => {
    let tt = t;
    if (tt < 0) tt += 1;
    if (tt > 1) tt -= 1;
    if (tt < 1 / 6) return p + (q - p) * 6 * tt;
    if (tt < 1 / 2) return q;
    if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
    return p;
  };
  return { r: hue(h + 1 / 3) * 255, g: hue(h) * 255, b: hue(h - 1 / 3) * 255 };
}

/** 十六进制 → HSL */
export function hexToHsl(hex: string): Hsl {
  return rgbToHsl(parseHex(hex));
}

/** HSL → 十六进制 */
export function hslToHex(hsl: Hsl): string {
  return toHex(hslToRgb(hsl));
}

/** 只改明度与饱和度，色相锁定（色阶不漂移的关键） */
export function withL(hsl: Hsl, l: number, s?: number, h?: number): string {
  return hslToHex({ h: h ?? hsl.h, s: clamp(s ?? hsl.s, 0, 100), l: clamp(l, 0, 100) });
}

/** RGB 线性插值（t=0 取 a，t=1 取 b） */
export function blend(a: string, b: string, t: number): string {
  const ca = parseHex(a);
  const cb = parseHex(b);
  const k = clamp(t, 0, 1);
  return toHex({
    r: ca.r + (cb.r - ca.r) * k,
    g: ca.g + (cb.g - ca.g) * k,
    b: ca.b + (cb.b - ca.b) * k,
  });
}

/** WCAG 相对亮度 */
export function luminance(hex: string): number {
  const { r, g, b } = parseHex(hex);
  const channel = (value: number): number => {
    const v = value / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/** WCAG 对比度（1~21） */
export function contrast(fg: string, bg: string): number {
  const lf = luminance(fg);
  const lb = luminance(bg);
  const lighter = Math.max(lf, lb);
  const darker = Math.min(lf, lb);
  return (lighter + 0.05) / (darker + 0.05);
}

/** 保留两位小数，报告输出用 */
export function round2(value: number): number {
  return Math.round(value * 100) / 100;
}

/**
 * 把前景色沿明度轴推到「与背景对比度 ≥ ratio」的最近位置。
 *
 * 背景偏亮 → 压暗前景；背景偏暗 → 提亮前景。逐 1% 明度步进，
 * 所以得到的一定是**最近的达标值**（需求里的"自动修正到最近达标值"）。
 * 极端情况下（背景压在中灰上）退化到纯黑/纯白兜底。
 */
export function ensureContrast(fg: string, bg: string, ratio: number): string {
  if (contrast(fg, bg) >= ratio) {
    return fg;
  }
  const hsl = hexToHsl(fg);
  const bgLum = luminance(bg);
  // 首选方向沿用老规则：亮底压暗、暗底提亮。
  // 但「中间调底色」（相对亮度约 0.18~0.40）两个方向都可能够不到 —— 例如浅驼 #BFA475
  // 上去提亮只会走到纯白，白字对比度仅 2.39。gallery 方案的中间调基色会大量落在这个区间，
  // 所以首选方向够不到时必须反向再走一遍，两个方向都取不到才退化到纯黑/纯白里更优的那个。
  const preferDarker = bgLum > 0.4;
  const walk = (darker: boolean): string | null => {
    for (let step = 1; step <= 100; step += 1) {
      const l = darker ? hsl.l - step : hsl.l + step;
      if (l < 0 || l > 100) break;
      const candidate = withL(hsl, l);
      if (contrast(candidate, bg) >= ratio) {
        return candidate;
      }
    }
    return null;
  };
  const preferred = walk(preferDarker);
  if (preferred) {
    return preferred;
  }
  const opposite = walk(!preferDarker);
  if (opposite) {
    return opposite;
  }
  return contrast('#000000', bg) >= contrast('#ffffff', bg) ? '#000000' : '#ffffff';
}

/* ============================================================================
 * 二、色阶推导
 * ========================================================================== */

/**
 * 生成亮色模式 brand 色阶 1~10。
 *
 * 两个锚点：`brand-5` 用色板给定的浅色（原值不动的浅色），`brand-7` 用色板主色。
 * 1~4 级在 brand-5 之上按明度阶梯铺开，6 级取 5/7 的明度中点，8~10 级按主色压暗。
 * 色相全程锁定主色色相，饱和度只在最浅的四级略微收敛（避免近白色发脏）。
 */
export function brandScaleLight(brand: string, brandLight: string): string[] {
  const main = hexToHsl(brand);
  const light = hexToHsl(brandLight);
  // brand-5 始终使用**用户给定的浅色**（配色方案必须忠于给定色板）。
  // 若给定浅色比主色还暗（脏数据），仍以给定值为准，只保证 1~4 级在它之上铺开。
  const l5 = clamp(light.l, 8, 97);
  // 1~4 级相对 brand-5 递增提亮（而不是用固定阶梯），这样即使 brandLight 很亮（L>82%），
  // brand-1..5 依然是从亮到暗的层次，不会出现 brand-4 比 brand-5 更暗的逆序。
  const l4 = clamp(l5 + 2, 10, 98);
  const l3 = clamp(l5 + 5, 12, 99);
  const l2 = clamp(l5 + 8, 14, 99.5);
  const l1 = clamp(l5 + 12, 16, 99.8);
  const ladder = [
    l1,
    l2,
    l3,
    l4,
    l5,
    // 6 级取 5/7 中点，但必须严格夹在两者之间
    clamp((l5 + main.l) / 2, Math.min(l5, main.l) + 1, Math.max(l5, main.l) - 1),
    main.l,
    main.l - 9,
    main.l - 18,
    main.l - 27,
  ];
  return ladder.map((l, index) => {
    if (index === 4) {
      // 无条件使用给定浅色（不再要求 light.l === l5，那是旧实现的限制）
      return brandLight.toLowerCase();
    }
    if (index === 6) {
      return brand.toLowerCase();
    }
    // 1~3 级做轻微去饱和，贴近底色时不刺眼
    const s = index <= 2 ? main.s * 0.72 : index === 3 ? main.s * 0.86 : main.s;
    return withL(main, clamp(l, 4, 99), s);
  });
}

/**
 * 生成深色模式 brand 色阶 1~10。
 *
 * 与 TDesign 深色语义一致：1 级最深（选中底/浅面），7 级主色，8~10 级比主色更亮。
 * 主色必须**提亮**（深底上原色会糊掉），提亮幅度取 +14 L，并保证与深色页面底
 * 至少 3.0 的对比度（UI 组件可辨底线）。
 */
export function brandScaleDark(brand: string, pageBg: string): string[] {
  const main = hexToHsl(brand);
  let l7 = clamp(main.l + 14, 44, 62);
  let mainDark = withL(main, l7);
  // 深底可辨底线：不足 3.0 继续提亮（最多到 78）
  while (contrast(mainDark, pageBg) < 3 && l7 < 78) {
    l7 += 1;
    mainDark = withL(main, l7);
  }
  const ladder = [l7 - 32, l7 - 27, l7 - 22, l7 - 17, l7 - 12, l7 - 6, l7, l7 + 7, l7 + 14, l7 + 21];
  return ladder.map((l, index) => withL(main, clamp(l, 10, 92), index < 6 ? main.s * 0.92 : main.s));
}

/* ============================================================================
 * 三、整套变量生成（含对比度自动修正与告警）
 * ========================================================================== */

/** 单条对比度检查结果 */
export interface ContrastCheck {
  /** 检查项键（报告表头用） */
  key: string;
  /** 人类可读标签 */
  label: string;
  /** 前景色 */
  fg: string;
  /** 背景色 */
  bg: string;
  /** 实测对比度 */
  ratio: number;
  /** 门槛 */
  min: number;
  /** 是否达标 */
  ok: boolean;
  /** 是否因为不达标被自动修正过 */
  adjusted: boolean;
}

/** 一套方案的完整产物 */
export interface PaletteBuild {
  /** 方案 */
  palette: PaletteDef;
  /** 模式 */
  mode: ColorMode;
  /** 变量表（变量名 → 值） */
  vars: Record<string, string>;
  /** 对比度检查 */
  checks: ContrastCheck[];
  /** 推过程中的告警（自动修正、色值推断等） */
  warnings: string[];
}

/** 10 档图表色的色相偏移（0 = 主色本身，其余围绕主色展开） */
const CHART_HUE_OFFSETS = [0, 26, -28, 58, 128, 186, 232, 286, 322, 94];

/**
 * 生成某套方案 × 某个模式的全部 CSS 变量。
 *
 * 浅色：底色用交融色；容器色向白色提亮；文字色由主色色相压暗并做 AA 修正。
 * 深色：交融色压到 L≈11% 作页面底；容器色向上提亮一档；文字色提亮并做 AA 修正；
 *       品牌色提亮（见 brandScaleDark），颜色永远不跨模式复用。
 */
export function buildPalette(paletteId: string, mode: ColorMode): PaletteBuild {
  const palette = getPalette(paletteId);
  const warnings: string[] = [];
  const mainHsl = hexToHsl(palette.brand);
  const mergeHsl = hexToHsl(palette.merge);
  const borderHsl = hexToHsl(palette.border);

  // ---------- 页面底色与中性色面 ----------
  let page: string;
  let container: string;
  let secondaryContainer: string;
  let component: string;
  let specialComponent: string;
  let stroke: string;
  let borderColor: string;

  if (mode === 'light') {
    page = palette.merge.toLowerCase();
    // 容器向白色提亮 62%：保证卡片比页面底更"白"，层次来自明度而不是灰
    container = blend(palette.merge, '#ffffff', 0.62);
    secondaryContainer = blend(container, palette.merge, 0.34);
    component = blend(container, palette.merge, 0.2);
    specialComponent = blend(container, '#ffffff', 0.4);
    stroke = blend(palette.border, container, 0.42);
    borderColor = palette.border.toLowerCase();
  } else {
    // 交融色压暗到 L≈11%，保留一点原色相，深底才有"这套方案"的味道
    page = withL(mergeHsl, 11, clamp(mergeHsl.s * 1.6, 6, 26), mainHsl.h);
    container = withL(mergeHsl, 16, clamp(mergeHsl.s * 1.3, 4, 18), mainHsl.h);
    secondaryContainer = withL(mergeHsl, 21, clamp(mergeHsl.s, 3, 14), mainHsl.h);
    component = withL(mergeHsl, 24, clamp(mergeHsl.s, 3, 12), mainHsl.h);
    specialComponent = withL(mergeHsl, 13, clamp(mergeHsl.s, 4, 16), mainHsl.h);
    // 深色模式下边框要比底亮一点才看得见
    stroke = withL(borderHsl, 26, clamp(borderHsl.s * 0.7, 4, 18), mainHsl.h);
    borderColor = withL(borderHsl, 34, clamp(borderHsl.s * 0.8, 5, 20), mainHsl.h);
  }
  // 悬停/激活：浅色是往底色方向压一点，深色是往白色方向提一点（深底"越交互越亮"）
  const containerHover = mode === 'light' ? blend(container, page, 0.32) : blend(container, '#ffffff', 0.06);
  const containerActive = mode === 'light' ? blend(container, page, 0.5) : blend(container, '#ffffff', 0.1);

  // ---------- 文字色：由主色色相压暗 / 提亮，并逐项做 AA 修正 ----------
  const textSeedHue = mainHsl.h;
  const textSeedSat = clamp(mainHsl.s * 0.28, 6, 22);
  const textSeedL = mode === 'light' ? 20 : 92;
  const textPrimaryRaw = hslToHex({ h: textSeedHue, s: textSeedSat, l: textSeedL });
  const textSecondaryRaw = hslToHex({ h: textSeedHue, s: textSeedSat, l: mode === 'light' ? 33 : 76 });
  const textPlaceholderRaw = hslToHex({ h: textSeedHue, s: textSeedSat, l: mode === 'light' ? 52 : 58 });

  const textPrimary = ensureContrast(textPrimaryRaw, page, 7);
  const textSecondary = ensureContrast(textSecondaryRaw, page, 4.5);
  // 占位/空状态文字：TDesign 还会把它用在表头、面包屑、descriptions 标签上，
  // 而这些场景的底色是「容器」（比页面底更亮/更暗），所以页面底与容器底都要 ≥3.0
  const textPlaceholder = ensureContrast(ensureContrast(textPlaceholderRaw, page, 3), container, 3);
  // 禁用/空状态文字：TDesign 的「暂无数据」也走这一档，所以同样按容器底 ≥3.0 兜底
  const textDisabled = ensureContrast(
    ensureContrast(hslToHex({ h: textSeedHue, s: textSeedSat, l: mode === 'light' ? 68 : 44 }), page, 2),
    container,
    3,
  );

  // ---------- brand 色阶 ----------
  const scaleLight = brandScaleLight(palette.brand, palette.brandLight);
  const scale =
    mode === 'light' ? scaleLight : brandScaleDark(palette.brand, page);
  const brandMain = scale[6];
  // 强调色/图表色的可见度底线：WCAG 1.4.11 对"有意义的图形"要求 ≥3，这里取 2.5（色块面积大的折线/柱）
  const SURFACE_MIN = 2.5;
  const accentSafe = ensureContrast(palette.accent, page, SURFACE_MIN);

  // ---------- 品牌文字色（链接 / 选中文字）：页面底与容器底都要 AA ----------
  // 链接既可能落在页面底，也可能落在卡片（容器底）上，两处都要达标，
  // 因此连续修正两次（第二次只会朝同方向继续走，不会破坏第一次的结果）。
  const brandText = ensureContrast(ensureContrast(brandMain, page, 4.5), container, 4.5);

  // ---------- 主交互面与「主色上的文字」----------
  // TDesign 的 `--td-brand-color` 一变量两用：既是主按钮底色，又是链接/选中页签的文字色。
  // 两种用法对明度的要求相反（底要深才配白字，字要亮才在深底上可辨），所以：
  //   · 浅色模式：白字不达标时，主交互面下移到压暗版（满足"白字按钮可读"），压暗后的颜色
  //     在浅底上作文字也天然达标；
  //   · 深色模式：主交互面保持**提亮后**的主色（要求 2：深底上品牌色必须提亮），
  //     白字改由 `--app-on-brand`（深色油墨）承担 —— 白底字改成深底字，两种模式都 ≥ 4.5。
  const whiteOnBrand = contrast('#ffffff', brandMain);
  const brandStrong = ensureContrast(brandMain, '#ffffff', 4.5);
  const brandStrongNeeded = whiteOnBrand < 4.5;
  if (brandStrongNeeded) {
    warnings.push(
      `${palette.name}（${mode === 'light' ? '浅色' : '深色'}）：白字在主色 ${brandMain} 上对比度 ${round2(
        whiteOnBrand,
      )} < 4.5，已派生压暗版 --app-brand-strong = ${brandStrong}（白字对比度 ${round2(
        contrast('#ffffff', brandStrong),
      )}）；${
        mode === 'light'
          ? '主按钮底色自动下移到压暗版'
          : '深色模式下按钮文字改用 --app-on-brand 深色油墨'
      }`,
    );
  }

  // 主交互面统一用压暗版：深色模式下若仍用主色，白字与深油墨都可能压不到 4.5
  //（例：墨莓深色主色 #a3738a 白字仅 3.91、深油墨上限约 4.3）。
  const brandSurface = brandStrongNeeded ? brandStrong : brandMain;
  // 主色上的文字色：优先白字，白字不达标则用与主色同色相的深色油墨
  const onBrand =
    contrast('#ffffff', brandSurface) >= 4.5
      ? '#ffffff'
      : ensureContrast(hslToHex({ h: mainHsl.h, s: clamp(mainHsl.s, 24, 62), l: 12 }), brandSurface, 4.5);
  const surfaceHsl = hexToHsl(brandSurface);
  // 深色模式"越交互越亮"，浅色模式"越交互越深"
  const brandSurfaceHover =
    mode === 'dark' ? withL(surfaceHsl, surfaceHsl.l + 8) : withL(surfaceHsl, surfaceHsl.l - 6);
  const brandSurfaceActive =
    mode === 'dark' ? withL(surfaceHsl, surfaceHsl.l + 14) : withL(surfaceHsl, surfaceHsl.l - 12);
  // 品牌色落在「品牌浅底块」(brand-1) 上时的文字色：深色模式下 brand-1 是一块偏暗的面，
  // 直接用 brand-7 当文字会糊（实测 1.5 左右），必须再提亮一档。
  const onBrandTint = ensureContrast(brandMain, scale[0], 4.5);

  // 品牌渐变：起点用压暗版、中段再按白字 4.5 兜底 —— 渐变条上的白字必须能读
  // ── 横幅渐变（老大 2026-10-28 第三版要求）──
  // 起点 = 方案主色，终点 = 方案第 2 色（同色系较浅那个 palette.brandLight）。
  // 老大两次反馈"前段太浅"：最终方案 = 前 72% 纯主色，只在最后 28% 过渡到方案浅色。
  // 因为终点是浅色，白字在上面只有约 2.1，所以渐变上的文字改用同色相极深油墨 --app-on-gradient，
  // 并做两轮 ensureContrast：分别对起点(主色)与终点(浅色)都保证 ≥ 4.5。
  // 起点用压暗版 brandStrong：白字在其上恒 ≥4.5，且前段视觉更深（老大反馈"太浅"）。
  // 前 38% 保持纯压暗版 —— 标题/副标题都落在这段里，保证白字可读（青碧/翠绿主色太浅，
  // 若文字跨进主色段会掉到 2.3 不可读）。
  const gradientStart = brandStrong.toLowerCase();
  const gradientMid = palette.brand.toLowerCase();
  const gradientEnd = palette.brandLight.toLowerCase();
  // 渐变文字用白字：起点是压暗版 brandStrong（白字 ≥4.5），前 55% 区域文字均落在深色段。
  // 后段渐浅处不放文字（副标题也左对齐），浅色端只留图标。
  const onGradient = '#ffffff';
  // 图标/浅色端用的深色油墨（浅色段上白字不可见）。
  // gallery 里 4 色方案的「次暗」源色可能本身就很暗（渐变终点 = 次暗色，如 #B36D28），
  // 深油墨落在暗端只有 1.1:1 —— 老规则（深油墨）达标就保持原样，不达标才换白字/纯黑兜底，
  // 这样既有 22 套的输出一个字节都不会变。
  const gradientEndInk = withL(hexToHsl(palette.brand), 12, 30);
  const onGradientSoft =
    contrast(gradientEndInk, palette.brandLight) >= 4.5
      ? gradientEndInk
      : contrast('#ffffff', palette.brandLight) >= contrast('#000000', palette.brandLight)
        ? '#ffffff'
        : '#000000';

  // ---------- 图表色 ----------
  // 图表是"有意义的图形"，按 WCAG 1.4.11 不低于 2.5 的可见度底线；
  // 主色本身在亮底太浅时（青碧 / 翠绿）自动压深一档并告警，色相不变。
  const CHART_MIN = SURFACE_MIN;
  const chartPrimary = ensureContrast(brandMain, page, CHART_MIN);
  if (chartPrimary !== brandMain) {
    warnings.push(
      `${palette.name}（${mode === 'light' ? '浅色' : '深色'}）：主色在页面底上仅 ${round2(
        contrast(brandMain, page),
      )} < ${CHART_MIN}，图表主色自动修正为 ${chartPrimary}（--td-brand-color-7 保持原值）`,
    );
  }
  const chartSoftL = mode === 'light' ? mainHsl.l + 18 : hexToHsl(brandMain).l + 12;
  const chartColors = CHART_HUE_OFFSETS.map((offset, index) => {
    if (index === 0) return chartPrimary;
    if (index === 1) return accentSafe;
    const hue = mainHsl.h + offset;
    const sat = clamp(mainHsl.s * 0.92, 18, 78);
    const lightness =
      index % 3 === 0 ? clamp(chartSoftL, 30, 86) : index % 3 === 1 ? clamp(mainHsl.l - 6, 16, 78) : clamp(hexToHsl(brandMain).l, 18, 82);
    return ensureContrast(hslToHex({ h: hue, s: sat, l: lightness }), page, CHART_MIN);
  });

  // ---------- 组装变量表 ----------
  const vars: Record<string, string> = {
    // brand 色阶
    '--td-brand-color-1': scale[0],
    '--td-brand-color-2': scale[1],
    '--td-brand-color-3': scale[2],
    '--td-brand-color-4': scale[3],
    '--td-brand-color-5': scale[4],
    '--td-brand-color-6': scale[5],
    '--td-brand-color-7': scale[6],
    '--td-brand-color-8': scale[7],
    '--td-brand-color-9': scale[8],
    '--td-brand-color-10': scale[9],
    // brand 派生语义（主交互面 = AA 安全色）
    '--td-brand-color': brandSurface,
    '--td-brand-color-light': scale[0],
    '--td-brand-color-focus': scale[1],
    '--td-brand-color-hover': brandSurfaceHover,
    '--td-brand-color-active': brandSurfaceActive,
    '--td-brand-color-disabled': scale[2],
    '--td-brand-color-light-hover': scale[1],
    '--td-brand-color-fade': scale[0],
    '--td-brand-color-focus-light': scale[0],

    // 背景 / 容器
    '--td-bg-color-page': page,
    '--td-bg-color-container': container,
    '--td-bg-color-container-hover': containerHover,
    '--td-bg-color-container-active': containerActive,
    '--td-bg-color-container-select': mode === 'light' ? scale[0] : scale[1],
    '--td-bg-color-secondarycontainer': secondaryContainer,
    '--td-bg-color-secondarycontainer-hover':
      mode === 'light' ? blend(secondaryContainer, page, 0.4) : blend(secondaryContainer, '#ffffff', 0.06),
    '--td-bg-color-secondarycontainer-active':
      mode === 'light' ? blend(secondaryContainer, page, 0.6) : blend(secondaryContainer, '#ffffff', 0.1),
    '--td-bg-color-component': component,
    '--td-bg-color-component-hover':
      mode === 'light' ? blend(component, page, 0.4) : blend(component, '#ffffff', 0.08),
    '--td-bg-color-component-active':
      mode === 'light' ? blend(component, page, 0.6) : blend(component, '#ffffff', 0.12),
    '--td-bg-color-component-disabled':
      mode === 'light' ? blend(component, page, 0.55) : blend(component, page, 0.45),
    '--td-bg-color-secondarycomponent': mode === 'light' ? blend(component, page, 0.3) : component,
    '--td-bg-color-secondarycomponent-hover':
      mode === 'light' ? blend(component, page, 0.5) : blend(component, '#ffffff', 0.1),
    '--td-bg-color-secondarycomponent-active':
      mode === 'light' ? blend(component, page, 0.65) : blend(component, '#ffffff', 0.16),
    '--td-bg-color-specialcomponent': specialComponent,

    // 描边
    '--td-component-stroke': stroke,
    '--td-component-border': borderColor,

    // 文字
    '--td-text-color-primary': textPrimary,
    '--td-text-color-secondary': textSecondary,
    '--td-text-color-placeholder': textPlaceholder,
    '--td-text-color-disabled': textDisabled,
    '--td-text-color-brand': brandText,
    '--td-text-color-link': brandText,
    '--td-text-color-anti': '#ffffff',

    // 图表
    '--td-chart-color': chartColors[0],
    ...Object.fromEntries(chartColors.map((color, index) => [`--td-chart-color-${index + 1}`, color])),

    // 投影：带一点主色，和整体色调呼应
    '--td-shadow-1': `0 1px 10px ${rgba(brandMain, 0.1)}`,
    '--td-shadow-2': `0 3px 6px -4px ${rgba(brandMain, 0.14)}, 0 6px 16px ${rgba(brandMain, 0.1)}`,
    '--td-shadow-3': `0 6px 30px 5px ${rgba(brandMain, 0.08)}, 0 16px 24px 2px ${rgba(brandMain, 0.06)}`,

    // 业务别名（现有代码里已经在用的 --app-* 全部覆盖）
    '--app-brand': brandSurface,
    '--app-brand-hover': brandSurfaceHover,
    '--app-brand-active': brandSurfaceActive,
    '--app-brand-light': scale[0],
    '--app-brand-border': scale[3],
    '--app-brand-accent': accentSafe,
    '--app-brand-strong': brandStrong,
    '--app-brand-strong-hover': withL(hexToHsl(brandStrong), hexToHsl(brandStrong).l - 6),
    '--app-brand-strong-active': withL(hexToHsl(brandStrong), hexToHsl(brandStrong).l - 12),
    // 主色**上面**的文字色：白字达标就是白，否则是同色相深油墨（深色模式下浅品牌底必须配深字）
    '--app-on-brand': onBrand,
    // 主色**浅底块**（brand-1）上的品牌文字色
    '--app-brand-on-tint': onBrandTint,
    '--app-palette-brand': palette.brand.toLowerCase(),
    '--app-palette-brand-light': palette.brandLight.toLowerCase(),
    '--app-palette-merge': palette.merge.toLowerCase(),
    '--app-palette-border': palette.border.toLowerCase(),
    '--app-palette-accent': palette.accent.toLowerCase(),
    '--app-bg-page': page,
    '--app-bg-container': container,
    '--app-border': borderColor,
    '--app-gradient-brand': `linear-gradient(135deg, ${gradientStart} 0%, ${gradientStart} 38%, ${gradientMid} 64%, ${gradientEnd} 100%)`,
    '--app-on-gradient': onGradient,
    '--app-on-gradient-soft': onGradientSoft,
    // 色卡 / 预览区用：5 个基准色原值（不做任何修正，永远等于需求色表）
    '--app-swatch-1': palette.brand.toLowerCase(),
    '--app-swatch-2': palette.brandLight.toLowerCase(),
    '--app-swatch-3': palette.merge.toLowerCase(),
    '--app-swatch-4': palette.border.toLowerCase(),
    '--app-swatch-5': palette.accent.toLowerCase(),
  };

  // ---------- 对比度检查（报告 = 本功能的测试证据） ----------
  const checks: ContrastCheck[] = [
    makeCheck('text-primary', '正文 / 页面底', textPrimary, page, 4.5, textPrimary !== textPrimaryRaw),
    makeCheck('text-secondary', '次要文字 / 页面底', textSecondary, page, 4.5, textSecondary !== textSecondaryRaw),
    makeCheck('text-placeholder', '占位文字 / 页面底', textPlaceholder, page, 3, textPlaceholder !== textPlaceholderRaw),
    makeCheck('text-placeholder-on-container', '占位文字 / 容器底', textPlaceholder, container, 3, textPlaceholder !== textPlaceholderRaw),
    makeCheck('text-on-container', '正文 / 容器底', textPrimary, container, 4.5, false),
    makeCheck('text-secondary-on-container', '次要文字 / 容器底', textSecondary, container, 4.5, false),
    makeCheck('brand-text', '品牌文字 / 页面底', brandText, page, 4.5, brandText !== brandMain),
    makeCheck('brand-text-on-container', '品牌文字 / 容器底', brandText, container, 4.5, brandText !== brandMain),
    makeCheck('white-on-brand-label', '白字 / 主色（brand-7）', '#ffffff', brandMain, 4.5, false),
    makeCheck('on-brand-surface', '主色上的文字 / 主交互面', onBrand, brandSurface, 4.5, brandStrongNeeded),
    makeCheck('brand-on-tint', '品牌文字 / 品牌浅底块', onBrandTint, scale[0], 4.5, onBrandTint !== brandMain),
    makeCheck('chart-1-on-page', '图表主色 / 页面底', chartPrimary, page, 2.5, chartPrimary !== brandMain),
    makeCheck('chart-2-on-page', '图表强调色 / 页面底', chartColors[1], page, 2.5, accentSafe !== palette.accent),
    // 文字区锁在 0~38% 的纯压暗版段（白字 ≥4.5）；主色段(38~64%)与浅色端只作视觉过渡，
    // 浅色段若放文字则由深色油墨承担（如图标）。
    makeCheck('on-gradient-start', '白字 / 渐变起点(压暗版)', onGradient, gradientStart, 4.5, true),
    makeCheck('on-gradient-end', '深油墨 / 渐变终点(浅色)', onGradientSoft, gradientEnd, 4.5, true),
  ];

  return { palette, mode, vars, checks, warnings };
}

function makeCheck(
  key: string,
  label: string,
  fg: string,
  bg: string,
  min: number,
  adjusted: boolean,
): ContrastCheck {
  const ratio = round2(contrast(fg, bg));
  return { key, label, fg, bg, ratio, min, ok: ratio >= min, adjusted };
}

/** 转成 CSS `rgb(r g b / a)`（现代语法，TDesign 同款） */
export function rgba(hex: string, alpha: number): string {
  const { r, g, b } = parseHex(hex);
  return `rgb(${r} ${g} ${b} / ${Math.round(alpha * 100)}%)`;
}

/** 变量表 → CSS 声明块文本（`--x: y;` 每行一条） */
export function varsToCss(vars: Record<string, string>, selector = ':root'): string {
  const body = Object.entries(vars)
    .map(([name, value]) => `  ${name}: ${value};`)
    .join('\n');
  return `${selector} {\n${body}\n}\n`;
}

/** 全部 6×2 组合的构建结果（脚本与设置页共用） */
export function buildAllPalettes(): PaletteBuild[] {
  const list: PaletteBuild[] = [];
  for (const palette of PALETTES) {
    for (const mode of ['light', 'dark'] as ColorMode[]) {
      list.push(buildPalette(palette.id, mode));
    }
  }
  return list;
}
