import 'package:flex_color_scheme/flex_color_scheme.dart';
import 'package:flutter/gestures.dart';
import 'package:material_ui/material_ui.dart';
import 'package:shiori/shiori.dart';

import 'reading_shelf_case.dart';

void main() => runApp(const ShelfPreview());

/// Direct preview of the production shelf and page transition.
class ShelfPreview extends StatefulWidget {
  const ShelfPreview({super.key});

  @override
  State<ShelfPreview> createState() => _ShelfPreviewState();
}

class _ShelfPreviewState extends State<ShelfPreview> {
  ThemeMode _themeMode = ThemeMode.system;

  ThemeData theme(Brightness brightness) => buildShioriTheme(
    brightness: brightness,
    colors: brightness == Brightness.light
        ? FlexColor.schemesList[2].light
        : FlexColor.schemesList[2].dark,
    tintSurfaces: false,
    blendLevel: 10,
    density: ShioriDensity.phone,
  );

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: '页间 · 书架动画预览',
    debugShowCheckedModeBanner: false,
    scrollBehavior: const MaterialScrollBehavior().copyWith(
      dragDevices: const {
        PointerDeviceKind.touch,
        PointerDeviceKind.mouse,
        PointerDeviceKind.trackpad,
        PointerDeviceKind.stylus,
      },
    ),
    theme: theme(Brightness.light),
    darkTheme: theme(Brightness.dark),
    themeMode: _themeMode,
    home: ReadingShelfCase(
      onToggleTheme: (brightness) => setState(() {
        _themeMode = brightness == Brightness.light
            ? ThemeMode.dark
            : ThemeMode.light;
      }),
    ),
  );
}
