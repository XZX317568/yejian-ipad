import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';
import 'package:mangayomi/services/archive_reader/archive_page.dart';

/// Supports the existing synchronous ImageProvider consumer API while archive
/// extraction remains asynchronous and page-sized.
@immutable
class ArchivePageImageProvider extends ImageProvider<ArchivePageImageProvider> {
  const ArchivePageImageProvider(this.page);

  final ArchivePage page;

  @override
  Future<ArchivePageImageProvider> obtainKey(
    ImageConfiguration configuration,
  ) => SynchronousFuture(this);

  @override
  ImageStreamCompleter loadImage(
    ArchivePageImageProvider key,
    ImageDecoderCallback decode,
  ) => MultiFrameImageStreamCompleter(
    codec: _load(decode),
    scale: 1,
    debugLabel: page.entryName,
  );

  Future<ui.Codec> _load(ImageDecoderCallback decode) async {
    try {
      final path = await page.resolveFilePath();
      return await decode(await ui.ImmutableBuffer.fromFilePath(path));
    } catch (_) {
      PaintingBinding.instance.imageCache.evict(this);
      rethrow;
    }
  }

  @override
  bool operator ==(Object other) =>
      other is ArchivePageImageProvider &&
      page.cachePath == other.page.cachePath;

  @override
  int get hashCode => page.cachePath.hashCode;
}
