import 'dart:typed_data';

import 'package:flex_color_scheme/flex_color_scheme.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi/l10n/generated/app_localizations.dart';
import 'package:mangayomi/l10n/l10n_delegates.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/modules/history/providers/isar_providers.dart';
import 'package:mangayomi/modules/library/providers/library_state_provider.dart';
import 'package:mangayomi/modules/library/widgets/library_archive_shelf.dart';
import 'package:mangayomi/modules/manga/detail/providers/state_providers.dart';
import 'package:material_ui/material_ui.dart';
import 'package:shiori/shiori.dart';

// A 1×1 transparent PNG; the shelf takes its real custom-cover branch without
// a network request.
final _pixel = Uint8List.fromList(const [
  0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, //
  0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
  0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
  0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
  0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
]);

Manga _book(int id, String name) => Manga(
  id: id,
  source: 'Fixture',
  author: '作者',
  artist: '',
  genre: const [],
  imageUrl: '',
  lang: 'zh',
  link: '/fixture/$id',
  name: name,
  status: Status.ongoing,
  description: '',
  sourceId: 1,
  isLocalArchive: true,
  customCoverImage: _pixel,
  favorite: true,
);

void main() {
  History history(int mangaId, String date) => History(
    itemType: ItemType.manga,
    chapterId: mangaId * 100,
    mangaId: mangaId,
    date: date,
  );

  test('the newest visible series wins when A is read again after B', () {
    final rows = [history(1, '100'), history(2, '200'), history(1, '300')];
    expect(latestVisibleHistoryMangaId(rows, visibleMangaIds: {1, 2}), 1);
  });

  test('history for a series outside the visible library is skipped', () {
    final rows = [history(1, '100'), history(2, '200'), history(3, '300')];
    expect(latestVisibleHistoryMangaId(rows, visibleMangaIds: {1, 2}), 2);
    expect(latestVisibleHistoryMangaId(rows, visibleMangaIds: {}), isNull);
  });

  testWidgets('the full library is one shelf and long-press selects a book', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 1;
    tester.view.physicalSize = const Size(390, 844);
    addTearDown(tester.view.reset);
    final container = ProviderContainer(
      overrides: [
        getAllHistoryStreamProvider.overrideWith(
          (ref, args) => Stream.value(<History>[]),
        ),
      ],
    );
    addTearDown(container.dispose);
    final books = [_book(1, '山海之间'), _book(2, '深蓝来信')];

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: MaterialApp(
          locale: const Locale('zh'),
          localizationsDelegates: appLocalizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          theme: buildShioriTheme(
            brightness: Brightness.light,
            colors: FlexColor.schemesList[2].light,
            tintSurfaces: false,
            blendLevel: 10,
            density: ShioriDensity.phone,
          ),
          home: Scaffold(
            body: Consumer(
              builder: (context, ref, _) => LibraryArchiveShelf(
                itemType: ItemType.manga,
                entries: books,
                selectedIds: ref.watch(mangasListStateProvider),
              ),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.byType(ShioriReadingShelf), findsOneWidget);
    expect(find.text('书架'), findsOneWidget);
    expect(find.text('01 / COLLECTION'), findsOneWidget);
    expect(tester.takeException(), isNull);

    final open = find.byKey(const ValueKey('archive-shelf-open'));
    await tester.ensureVisible(open);
    await tester.pumpAndSettle();
    await tester.longPress(open);
    await tester.pumpAndSettle();
    expect(container.read(isLongPressedStateProvider), isTrue);
    expect(container.read(mangasListStateProvider), contains(1));
    expect(find.byKey(const ValueKey('archive-selected-1')), findsWidgets);
    await tester.tap(open);
    await tester.pumpAndSettle();
    expect(container.read(mangasListStateProvider), isEmpty);
  });
}
