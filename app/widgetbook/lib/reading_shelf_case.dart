import 'dart:ui' as ui;

import 'package:material_ui/material_ui.dart';
import 'package:shiori/shiori.dart';

/// Original generated covers and sample reading data, only for the catalogue.
/// The production shelf receives real history and image providers from Isar.
class ReadingShelfCase extends StatefulWidget {
  const ReadingShelfCase({super.key, this.onToggleTheme});

  final ValueChanged<Brightness>? onToggleTheme;

  @override
  State<ReadingShelfCase> createState() => _ReadingShelfCaseState();
}

class _ReadingShelfCaseState extends State<ReadingShelfCase> {
  final List<ImageProvider> _covers = [];
  bool _searching = false;
  String _query = '';
  static const _titles = ['山海之间', '深蓝来信', '雨后列车', '小镇回声', '月光旅人'];
  static const _colors = [
    Color(0xFF345753),
    Color(0xFF284B6C),
    Color(0xFF98764D),
    Color(0xFF685879),
    Color(0xFFB65F4A),
  ];

  @override
  void initState() {
    super.initState();
    _makeCovers();
  }

  Future<void> _makeCovers() async {
    for (var i = 0; i < _colors.length; i++) {
      final recorder = ui.PictureRecorder();
      final canvas = Canvas(recorder);
      canvas.drawColor(_colors[i], BlendMode.src);
      canvas.drawCircle(
        const Offset(185, 130),
        88,
        Paint()..color = const Color(0xFFECE9DA).withValues(alpha: 0.85),
      );
      for (var j = 0; j < 6; j++) {
        final path = Path()
          ..moveTo(0, 220 + j * 20)
          ..quadraticBezierTo(155, 90 + j * 38, 320, 290 + j * 18)
          ..lineTo(320, 480)
          ..lineTo(0, 480)
          ..close();
        canvas.drawPath(
          path,
          Paint()
            ..color = Color.lerp(_colors[i], const Color(0xFF0E242B), j / 7)!,
        );
      }
      final text = TextPainter(
        text: TextSpan(
          text: 'YEJIAN\nCOLLECTION\n\n0${i + 1}',
          style: const TextStyle(
            color: Color(0xFFF6F2E8),
            fontSize: 24,
            fontWeight: FontWeight.w600,
            height: 1.5,
            letterSpacing: 2,
          ),
        ),
        textDirection: TextDirection.ltr,
      )..layout(maxWidth: 260);
      text.paint(canvas, const Offset(28, 270));
      text.dispose();
      final picture = recorder.endRecording();
      final image = await picture.toImage(320, 480);
      final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
      if (bytes != null) _covers.add(MemoryImage(bytes.buffer.asUint8List()));
      image.dispose();
      picture.dispose();
    }
    if (mounted) setState(() {});
  }

  void _open(int index) {
    Navigator.of(context).push<void>(
      ShioriPaperPage<void>(
        child: _SampleDetail(
          index: index,
          title: _titles[index],
          image: index < _covers.length ? _covers[index] : null,
        ),
      ).createRoute(context),
    );
  }

  void _showNotes() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(24, 8, 24, 32),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text('书架动画预览', style: context.shiori.text.seriesTitle),
              const SizedBox(height: 16),
              Text(
                '左右拖动切书，点击中央封面打开详情。返回时，封面会落回原位。\n\n'
                '这里使用原创示例封面与阅读记录，书架、导航和纸张动画与应用共用组件。',
                style: context.shiori.text.body,
              ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      toolbarHeight: ShioriArchiveMasthead.height,
      title: _searching
          ? TextField(
              autofocus: true,
              onChanged: (value) => setState(() => _query = value),
              decoration: const InputDecoration(
                hintText: '搜索示例书库',
                border: InputBorder.none,
              ),
            )
          : const ShioriArchiveMasthead(
              title: '页间',
              code: 'YEJIAN / READING ARCHIVE',
            ),
      actions: [
        IconButton(
          tooltip: _searching ? '关闭搜索' : '搜索',
          onPressed: () => setState(() {
            _searching = !_searching;
            if (!_searching) _query = '';
          }),
          icon: Icon(
            _searching ? Symbols.close_rounded : Symbols.search_rounded,
          ),
        ),
      ],
    ),
    body: ListView(
      children: [
        ShioriReadingShelf(
          title: '书架',
          sectionCode: '01 / COLLECTION',
          openLabel: '打开',
          entries: [
            for (var i = 0; i < _titles.length; i++)
              if (_titles[i].contains(_query))
                ShioriReadingEntry(
                  id: i,
                  title: _titles[i],
                  subtitle: '第 ${i + 3} 话 · 第 12 页',
                  image: i < _covers.length ? _covers[i] : null,
                  progress: (i + 1) / 8,
                  heroTag: 'sample-cover-$i',
                  onOpen: () => _open(i),
                ),
          ],
        ),
        if (!_titles.any((title) => title.contains(_query)))
          const Padding(
            padding: EdgeInsets.all(24),
            child: Center(child: Text('没有找到这本书')),
          ),
        Padding(
          padding: EdgeInsets.all(ShioriSpace.s24),
          child: Text(
            '动画预览 · 原创示例封面与阅读记录',
            textAlign: TextAlign.center,
            style: context.shiori.caption,
          ),
        ),
      ],
    ),
    bottomNavigationBar: widget.onToggleTheme == null
        ? null
        : ShioriArchiveDock(
            currentIndex: _searching ? 1 : 0,
            destinations: [
              const NavigationDestination(
                icon: Icon(Symbols.book_rounded),
                label: '书架',
              ),
              const NavigationDestination(
                icon: Icon(Symbols.search_rounded),
                label: '搜索',
              ),
              NavigationDestination(
                icon: Icon(
                  Theme.of(context).brightness == Brightness.light
                      ? Symbols.dark_mode_rounded
                      : Symbols.light_mode_rounded,
                ),
                label: '主题',
              ),
              const NavigationDestination(
                icon: Icon(Symbols.info_rounded),
                label: '说明',
              ),
            ],
            onSelected: (index) {
              switch (index) {
                case 0:
                case 1:
                  setState(() {
                    _searching = index == 1;
                    if (!_searching) _query = '';
                  });
                case 2:
                  widget.onToggleTheme!(Theme.of(context).brightness);
                case 3:
                  _showNotes();
              }
            },
          ),
  );
}

class _SampleDetail extends StatelessWidget {
  const _SampleDetail({
    required this.index,
    required this.title,
    required this.image,
  });
  final int index;
  final String title;
  final ImageProvider? image;

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('书籍详情')),
    body: ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            ShioriPaperCoverHero(
              tag: 'sample-cover-$index',
              image: image,
              faceSize: const Size(110, 165),
              isDetail: true,
              child: SizedBox(
                width: 110,
                height: 165,
                child: ClipRRect(
                  borderRadius: BorderRadius.circular(3),
                  child: image == null
                      ? ColoredBox(color: context.shiori.color.ink2)
                      : Image(image: image!, fit: BoxFit.cover),
                ),
              ),
            ),
            const SizedBox(width: 24),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(title, style: context.shiori.text.screenTitle),
                  const SizedBox(height: 12),
                  Text(
                    '第 ${index + 3} 话 · 第 12 页',
                    style: context.shiori.caption,
                  ),
                  const SizedBox(height: 24),
                  Text('原创示例', style: context.shiori.text.body),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 32),
        Text('这是书架动画预览。返回书架可以查看封面落回原位的动作。', style: context.shiori.text.body),
      ],
    ),
  );
}
