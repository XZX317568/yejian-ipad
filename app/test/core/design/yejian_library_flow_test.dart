import 'dart:io';
import 'dart:convert';
import 'dart:ui' as ui;

import 'package:flex_color_scheme/flex_color_scheme.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';
import 'package:isar_community/isar.dart';
import 'package:material_ui/material_ui.dart';
import 'package:mangayomi/core/app_identity.dart';
import 'package:mangayomi/l10n/generated/app_localizations.dart';
import 'package:mangayomi/l10n/l10n_delegates.dart';
import 'package:mangayomi/models/category.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/download.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/models/track.dart';
import 'package:mangayomi/models/update.dart';
import 'package:mangayomi/modules/library/library_screen.dart';
import 'package:mangayomi/modules/library/providers/isar_providers.dart';
import 'package:mangayomi/modules/library/providers/library_filter_provider.dart';
import 'package:mangayomi/modules/history/providers/isar_providers.dart';
import 'package:mangayomi/modules/more/categories/providers/isar_providers.dart';
import 'package:mangayomi/modules/main_view/widgets/mobile_bottom_navigation.dart';
import 'package:mangayomi/modules/main_view/widgets/nav_destination_builders.dart';
import 'package:mangayomi/modules/main_view/widgets/tablet_layout.dart';
import 'package:mangayomi/repositories/db.dart';
import 'package:mangayomi/repositories/db_write_queue.dart';
import 'package:shiori/shiori.dart';

const _titles = ['山海之间', '深蓝来信', '雨后列车', '小镇回声', '月光旅人', '远山笔记'];

Future<Uint8List> _cover(int index) async {
  final colors = [
    const Color(0xFF345753),
    const Color(0xFF284B6C),
    const Color(0xFF98764D),
    const Color(0xFF685879),
    const Color(0xFFB65F4A),
    const Color(0xFF4E6776),
  ];
  final recorder = ui.PictureRecorder();
  final canvas = Canvas(recorder);
  canvas.drawColor(colors[index], BlendMode.src);
  canvas.drawCircle(
    const Offset(140, 100),
    62,
    Paint()..color = const Color(0xFFE9E5CF),
  );
  for (var j = 0; j < 6; j++) {
    canvas.drawPath(
      Path()
        ..moveTo(0, 145 + j * 18)
        ..quadraticBezierTo(120, 70 + j * 25, 240, 220 + j * 18)
        ..lineTo(240, 360)
        ..lineTo(0, 360)
        ..close(),
      Paint()
        ..color = Color.lerp(colors[index], const Color(0xFF102831), j / 7)!,
    );
  }
  final text = TextPainter(
    text: TextSpan(
      text: 'YEJIAN\nCOLLECTION\n\n0${index + 1}',
      style: const TextStyle(
        color: Color(0xFFF1EFE5),
        fontSize: 18,
        height: 1.5,
        fontFamily: 'packages/shiori/Figtree',
      ),
    ),
    textDirection: TextDirection.ltr,
  )..layout(maxWidth: 210);
  text.paint(canvas, const Offset(22, 205));
  text.dispose();
  final picture = recorder.endRecording();
  final image = await picture.toImage(240, 360);
  final data = await image.toByteData(format: ui.ImageByteFormat.png);
  image.dispose();
  picture.dispose();
  return data!.buffer.asUint8List();
}

void main() {
  late Directory directory;
  final capture = GlobalKey();
  setUpAll(() async {
    TestWidgetsFlutterBinding.ensureInitialized();
    await Isar.initializeIsarCore(download: true);
    final manifest =
        jsonDecode(await rootBundle.loadString('FontManifest.json')) as List;
    for (final family in manifest) {
      final loader = FontLoader(family['family'] as String);
      for (final asset in family['fonts'] as List) {
        loader.addFont(rootBundle.load(asset['asset'] as String));
      }
      await loader.load();
    }
    final font = File('C:/Windows/Fonts/msyh.ttc');
    if (font.existsSync()) {
      final data = ByteData.sublistView(await font.readAsBytes());
      for (final family in ['YejianVisualTest', 'Roboto']) {
        await (FontLoader(family)..addFont(Future.value(data))).load();
      }
    }
    final mono = File('C:/Windows/Fonts/consola.ttf');
    if (mono.existsSync()) {
      await (FontLoader('monospace')..addFont(
            Future.value(ByteData.sublistView(await mono.readAsBytes())),
          ))
          .load();
    }
  });
  setUp(() async {
    directory = await Directory('build/library-flow-tests')
        .create(recursive: true);
    directory = await directory.createTemp('library-');
    isar = await Isar.open(
      [
        MangaSchema,
        ChapterSchema,
        SettingsSchema,
        CategorySchema,
        SourceSchema,
        HistorySchema,
        DownloadSchema,
        UpdateSchema,
        TrackSchema,
      ],
      directory: directory.absolute.path,
      name: 'library_flow',
    );
    final covers = [for (var i = 0; i < _titles.length; i++) await _cover(i)];
    isar.writeTxnSync(() {
      isar.settings.putSync(newInstallSettings(isTvDevice: false));
      for (var i = 0; i < _titles.length; i++) {
        final manga = Manga(
          id: i + 1,
          name: _titles[i],
          favorite: true,
          source: 'Fixture',
          sourceId: 1,
          author: '',
          artist: '',
          genre: [],
          imageUrl: '',
          lang: 'zh',
          link: '/fixture/$i',
          status: Status.ongoing,
          description: '',
          isLocalArchive: true,
          customCoverImage: covers[i],
        );
        isar.mangas.putSync(manga);
        final chapter = Chapter(
          id: 100 + i,
          mangaId: manga.id,
          name: '第 ${i + 3} 话',
          lastPageRead: '12',
        )..manga.value = manga;
        isar.chapters.putSync(chapter);
        chapter.manga.saveSync();
        if (i < 3) {
          final history = History(
            id: i + 1,
            itemType: ItemType.manga,
            chapterId: chapter.id,
            mangaId: manga.id,
            date: '${1000 - i}',
          )..chapter.value = chapter;
          isar.historys.putSync(history);
          history.chapter.saveSync();
        }
      }
    });
  });
  tearDown(() async {
    await dbWriteQueue.run(() {});
    await isar.close(deleteFromDisk: true);
    await directory.delete(recursive: true);
  });

  Future<void> settle(WidgetTester tester) async {
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 100)),
      );
      await tester.pump(const Duration(milliseconds: 150));
    }
  }

  Future<void> screenshot(WidgetTester tester, String name) async {
    final boundary =
        capture.currentContext!.findRenderObject()! as RenderRepaintBoundary;
    await tester.runAsync(() async {
      final image = await boundary.toImage();
      final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
      await Directory('build/acceptance').create(recursive: true);
      await File('build/acceptance/$name.png')
          .writeAsBytes(bytes!.buffer.asUint8List());
      image.dispose();
    });
  }

  for (final size in [
    const Size(390, 844),
    const Size(1024, 768),
    const Size(375, 768),
  ]) {
    testWidgets('real library and search at ${size.width}', (tester) async {
      tester.view.devicePixelRatio = 1;
      tester.view.physicalSize = size;
      addTearDown(tester.view.reset);
      final nav = NavDestinationBuilders();
      const dest = ['/MangaLibrary', '/updates', '/browse', '/more'];
      late GoRouter router;
      router = GoRouter(
        initialLocation: '/MangaLibrary',
        routes: [
          GoRoute(
            path: '/MangaLibrary',
            builder: (context, state) => Consumer(
              builder: (context, ref, _) {
                const library = LibraryScreen(
                  itemType: ItemType.manga,
                  search: null,
                );
                return size.width >= 600
                    ? Scaffold(
                        body: MainTabletLayout(
                          isLongPressed: false,
                          location: '/MangaLibrary',
                          dest: dest,
                          currentIndex: 0,
                          route: router,
                          ref: ref,
                          buildNavigationWidgetsDesktop:
                              nav.buildNavigationWidgetsDesktop,
                          child: library,
                        ),
                      )
                    : Scaffold(
                        body: library,
                        bottomNavigationBar: MainMobileBottomNavigation(
                          isLongPressed: false,
                          location: '/MangaLibrary',
                          currentIndex: 0,
                          dest: dest,
                          route: router,
                          ref: ref,
                          buildNavigationWidgetsMobile:
                              nav.buildNavigationWidgetsMobile,
                          onDestinationSelected: (_) {},
                          useFloatingNav: true,
                        ),
                      );
              },
            ),
          ),
        ],
      );
      addTearDown(() async {
        await tester.pumpWidget(const SizedBox());
        router.dispose();
        await tester.pump();
      });
      await tester.pumpWidget(
        ProviderScope(
          // Native watcher notifications run outside the widget test's fake
          // clock. Render database snapshots through the same provider contract.
          overrides: [
            getSettingsStreamProvider.overrideWith(
              (ref) => Stream.value(isar.settings.getSync(227)!),
            ),
            getAllMangaStreamProvider.overrideWith(
              (ref, args) => Stream.value(isar.mangas.where().findAllSync()),
            ),
            getMangaCategorieStreamProvider.overrideWith(
              (ref, args) => Stream.value(<Category>[]),
            ),
            getAllHistoryStreamProvider.overrideWith(
              (ref, args) => Stream.value(isar.historys.where().findAllSync()),
            ),
            downloadedChapterIdsProvider.overrideWith(
              (ref) => Stream.value(<int>{}),
            ),
            trackedMangaIdsProvider.overrideWith(
              (ref) => Stream.value(<int>{}),
            ),
          ],
          child: MaterialApp.router(
            routerConfig: router,
            locale: const Locale('zh'),
            localizationsDelegates: appLocalizationsDelegates,
            supportedLocales: AppLocalizations.supportedLocales,
            theme: buildShioriTheme(
              brightness: Brightness.light,
              colors: FlexColor.schemesList[2].light,
              tintSurfaces: false,
              blendLevel: 10,
              density: ShioriDensity.phone,
              fontFamily: 'YejianVisualTest',
            ),
            builder: (context, child) => RepaintBoundary(
              key: capture,
              child: MediaQuery(
                data: MediaQuery.of(context).copyWith(
                  textScaler: TextScaler.linear(size.width == 375 ? 2 : 1),
                ),
                child: child!,
              ),
            ),
          ),
        ),
      );
      await settle(tester);
      expect(tester.takeException(), isNull);
      await screenshot(
        tester,
        size.width == 375
            ? 'split-large-text-library'
            : size.width < 600
            ? 'phone-library'
            : 'tablet-library',
      );
      expect(find.byType(ShioriReadingShelf), findsOneWidget);
      final shelf = tester.widget<ShioriReadingShelf>(
        find.byType(ShioriReadingShelf),
      );
      expect(shelf.entries.length, 6);
      expect(shelf.entries.map((entry) => entry.title), containsAll(_titles));
      await tester.tap(find.byIcon(Symbols.search_rounded).first);
      await settle(tester);
      await tester.enterText(find.byType(TextField).first, '不存在的书名');
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.byType(ShioriReadingShelf), findsNothing);
      expect(tester.takeException(), isNull);
    });
  }
}
