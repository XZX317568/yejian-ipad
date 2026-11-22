import 'package:material_ui/material_ui.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/modules/history/providers/isar_providers.dart';
import 'package:mangayomi/modules/library/widgets/library_entry_utils.dart';
import 'package:mangayomi/modules/manga/detail/providers/state_providers.dart';
import 'package:mangayomi/modules/widgets/cover_hero.dart';
import 'package:mangayomi/providers/l10n_providers.dart';
import 'package:mangayomi/repositories/chapter_repository.dart';
import 'package:shiori/shiori.dart';

/// The series whose newest history row is actually visible in the library.
///
/// A map keyed by manga id cannot answer this: writing a later row for an
/// existing key keeps the key's original insertion position. Scanning the
/// rows newest-first avoids that trap (A old, B middle, A new must pick A).
int? latestVisibleHistoryMangaId(
  List<History> rows, {
  required Set<int> visibleMangaIds,
}) {
  int when(History h) => int.tryParse(h.date ?? '') ?? 0;
  final sorted = [...rows]..sort((a, b) => when(b).compareTo(when(a)));
  for (final row in sorted) {
    final mangaId = row.mangaId;
    if (mangaId != null && visibleMangaIds.contains(mangaId)) {
      return mangaId;
    }
  }
  return null;
}

/// The whole manga collection as the paper shelf.
///
/// This is the library itself, not a "recently read" carousel: it shows every
/// entry the current filter, search and category leave visible, and its first
/// page is the series last read when there is one. Tapping a series opens its
/// real detail page; long-pressing enters the existing multi-selection mode.
class LibraryArchiveShelf extends ConsumerWidget {
  const LibraryArchiveShelf({
    super.key,
    required this.itemType,
    required this.entries,
    required this.selectedIds,
    this.onRefresh,
  });

  final ItemType itemType;
  final List<Manga> entries;
  final Set<int> selectedIds;
  final Future<void> Function()? onRefresh;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final l10n = context.l10n;
    final historyAsync = ref.watch(
      getAllHistoryStreamProvider(itemType: itemType),
    );
    final history =
        historyAsync.asData?.value ??
        (historyAsync.hasError ? const <History>[] : null);
    if (history == null) {
      // Do not open on the first book and then jump when history arrives:
      // the shelf reads initialId only when it is created.
      return ListView(
        physics: const NeverScrollableScrollPhysics(),
        children: [
          for (var i = 0; i < 3; i++) const ShioriSkeletonRow(),
        ],
      );
    }
    final latest = _latestHistory(history);
    final latestChapters = _chaptersByIds(latest.values.map((h) => h.chapterId));
    final selecting = ref.watch(isLongPressedStateProvider);
    final visibleIds = {
      for (final manga in entries)
        if (manga.id != null) manga.id!,
    };

    final shelfEntries = <ShioriReadingEntry>[];
    for (final manga in entries) {
      final id = manga.id;
      if (id == null) continue;
      shelfEntries.add(
        ShioriReadingEntry(
          id: id,
          title: manga.name ?? '',
          subtitle: _subtitle(
            context,
            manga,
            latest[manga.id],
            latestChapters,
          ),
          image: resolveCoverImage(manga, ref),
          heroTag: coverHeroTag(id),
          selected: selectedIds.contains(id),
          onOpen: () => _open(context, ref, manga),
          onLongPress: () =>
              handleLongOrSecondaryTap(selecting, ref, manga),
          onDetails: () => _open(context, ref, manga),
        ),
      );
    }

    final latestMangaId = latestVisibleHistoryMangaId(
      history,
      visibleMangaIds: visibleIds,
    );
    final initialId = latestMangaId ?? (shelfEntries.isEmpty
        ? null
        : shelfEntries.first.id);

    final shelf = ShioriReadingShelf(
      entries: shelfEntries,
      title: l10n.library,
      openLabel: l10n.read,
      sectionCode: '01 / COLLECTION',
      initialId: initialId,
      repeatFill: true,
      animateEntrance: true,
      selectionMode: selecting,
    );

    final refreshable = onRefresh;
    if (refreshable == null) return shelf;
    return RefreshIndicator(
      onRefresh: refreshable,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        children: [
          shelf,
          const SizedBox(height: ShioriSpace.s24),
        ],
      ),
    );
  }

  void _open(BuildContext context, WidgetRef ref, Manga manga) {
    if (ref.read(isLongPressedStateProvider)) {
      handleLongOrSecondaryTap(true, ref, manga);
      return;
    }
    // Same route and per-series setup the grid uses, including local archives.
    onTapEntry(
      isLongPressed: false,
      ref: ref,
      context: context,
      entry: manga,
    );
  }

  /// The newest history row for each series, for the subtitle. The shelf's
  /// opening book is chosen by [latestVisibleHistoryMangaId] instead.
  Map<int, History> _latestHistory(List<History> rows) {
    int when(History h) => int.tryParse(h.date ?? '') ?? 0;
    final sorted = [...rows]..sort((a, b) => when(a).compareTo(when(b)));
    final latest = <int, History>{};
    for (final row in sorted) {
      final mangaId = row.mangaId;
      if (mangaId != null) latest[mangaId] = row;
    }
    return latest;
  }

  Map<int, Chapter> _chaptersByIds(Iterable<int?> ids) {
    final resolved = ids.nonNulls.toList();
    if (resolved.isEmpty) return const {};
    return {
      for (final chapter in chapterRepository.getAllByIds(resolved).nonNulls)
        if (chapter.id != null) chapter.id!: chapter,
    };
  }

  String _subtitle(
    BuildContext context,
    Manga manga,
    History? history,
    Map<int, Chapter> chapters,
  ) {
    final l10n = context.l10n;
    final last = history == null ? null : chapters[history.chapterId];
    final page = int.tryParse(last?.lastPageRead ?? '') ?? 0;
    final where = last == null
        ? null
        : [
            last.name ?? '',
            if (page > 0) l10n.page(page.toString()),
          ].where((part) => part.isNotEmpty).join(' · ');
    return (where != null && where.isNotEmpty)
        ? where
        : (manga.author?.isNotEmpty ?? false)
        ? manga.author!
        : (manga.source ?? '');
  }
}
