# 页间阅读字体

`assets/YejianReadingSans-Regular.ttf` 用于正文，`assets/YejianReadingSerif-Semibold.ttf` 用于书名及标题。它们是下列官方开源字体的静态衍生版本，分别使用 400 和 600 字重，均按 SIL Open Font License 1.1 分发。原许可和版权声明原样保存在 `ReadingSans-OFL.txt`、`ReadingSerif-OFL.txt`。

## 原始来源

- Noto Sans SC：Google Fonts 官方仓库，commit `a85815a42757630ce188fdad368c2dfc444d4773`，目录 `ofl/notosanssc/`，原文件 `NotoSansSC[wght].ttf`。
  - 源码及字体：<https://github.com/google/fonts/tree/a85815a42757630ce188fdad368c2dfc444d4773/ofl/notosanssc>
  - Copyright 2014–2021 Adobe，Reserved Font Name 为 `Source`。
- Noto Serif SC：Google Fonts 官方仓库，commit `8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5`，目录 `ofl/notoserifsc/`，原文件 `NotoSerifSC[wght].ttf`。
  - 源码及字体：<https://github.com/google/fonts/tree/8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5/ofl/notoserifsc>
  - Copyright 2012 Google Inc. All Rights Reserved.

获取日期：2026-11-20。衍生字体在 name table 中使用 `Yejian Reading Sans`、`Yejian Reading Serif`；原版权、许可和来源记录保留。

## 修改与覆盖

用 fontTools 4.63.0 的 `instantiateVariableFont` 固定字重，移除变量轴；用 `Subsetter` 保留上游支持的全部基本多文种平面映射（U+0000–U+FFFF），移除 hinting 和未使用的字形布局变体。保留 `ccmp`、`locl`、`kern`、`mark`、`mkmk`、`liga`、`clig` 特性。egui 不使用垂直排版及可变字重。

这不是按当前界面文案制作的子集。简繁汉字、CJK 扩展 A、假名、拉丁字母、标点和符号均保留上游覆盖。未保留的辅助平面罕见字和其他缺字继续通过原有 `DroidSansFallback.ttf` 回退；回退本身覆盖 41,991 个 Unicode 字符，仍不能保证所有 Unicode 字符都有字形。

| 文件 | 字重 | 字符映射数 | 大小 |
| --- | ---: | ---: | ---: |
| YejianReadingSans-Regular.ttf | 400 | 30,445 | 10,250,668 字节 |
| YejianReadingSerif-Semibold.ttf | 600 | 30,486 | 14,404,192 字节 |

合计 24,654,860 字节；普通 ZIP/Deflate 压缩约 14.5 MB。正文和标题使用不同字形，无法依靠字体合集明显减少体积。

## 集成

`src/typography.rs` 安装正文、标题与原有后备字体。`configure(ctx)` 只修改字体集合，不修改 egui 文本大小、触控尺寸或用户字号；`display(size)` 返回标题的 `FontId`。原有等宽拉丁字体仍保留，中文缺字再回退阅读正文字体和 Droid。

客户端的 `configure_fonts` 调用此模块后，仍应应用原有 `text_styles` 和控件间距。fontTools 已重新解析两个输出文件，验证字重、静态字体表与全部预期 BMP 映射；简繁、假名及界面常用字的覆盖检查通过。实际 egui 渲染由生产 UI 验收确认。
