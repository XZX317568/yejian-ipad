// Physical shelf adapted from mikufoxxx/Wenku8_Flutter v0.5.6.
// Modified 2026-11-17 for Shiori and real manga entries.
// Original license/notices: third_party/wenku8; see WENKU8_ADAPTATION.md.
import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:material_ui/material_ui.dart';

import '../tokens.dart';
import 'archive_shelf/archive_motion.dart';
import 'archive_shelf/archive_scroll_simulation.dart';
import 'archive_shelf/archive_shelf_geometry.dart';
import 'archive_shelf/archive_shelf_loading.dart';
import 'archive_shelf/paper_cover.dart';

part 'archive_shelf/shelf_scene.dart';

@immutable
class ShioriReadingEntry {
  const ShioriReadingEntry({
    required this.id,
    required this.title,
    required this.subtitle,
    required this.onOpen,
    this.image,
    this.progress = 0,
    this.onDetails,
    this.onLongPress,
    this.heroTag,
    this.selected = false,
  });
  final Object id;
  final String title, subtitle;
  final ImageProvider? image;
  final double progress;
  final VoidCallback onOpen;
  final VoidCallback? onDetails, onLongPress;
  final Object? heroTag;
  final bool selected;
}

/// One continuous scroll clock drives the book's cover, spine and cap.
/// The host owns navigation and persistence; the shelf owns selection.
class ShioriReadingShelf extends StatefulWidget {
  const ShioriReadingShelf({
    super.key,
    required this.entries,
    required this.title,
    required this.openLabel,
    this.sectionCode = '01 / READING',
    this.initialId,
    this.repeatFill = true,
    this.animateEntrance = true,
    this.selectionMode = false,
  });
  final List<ShioriReadingEntry> entries;
  final String title, openLabel, sectionCode;
  final Object? initialId;
  final bool repeatFill, animateEntrance;
  final bool selectionMode;
  @override
  State<ShioriReadingShelf> createState() => _ReadingShelfState();
}

class _ReadingShelfState extends State<ShioriReadingShelf>
    with SingleTickerProviderStateMixin {
  ScrollController? _scroll;
  late final _WavePose _pose;
  late final ValueNotifier<int> _active;
  late final ValueNotifier<int> _window;
  late final AnimationController _entrance;
  int? _targetIndex;
  int _command = 0;
  bool _reduced = false, _hapticGesture = false;
  int? _lastDetent;

  @override
  void initState() {
    super.initState();
    final initial = math.max(
      0,
      widget.entries.indexWhere((entry) => entry.id == widget.initialId),
    );
    _entrance = AnimationController(
      vsync: this,
      duration: ShelfMaterial.entrance,
      value: widget.animateEntrance ? 0 : 1,
    );
    _pose = _WavePose(
      widget.entries.length,
      initial.toDouble(),
      _entrance,
      repeatFill: widget.repeatFill,
    );
    _entrance.addListener(_pose.inkChanged);
    _active = ValueNotifier(initial);
    _window = ValueNotifier(initial);
  }

  void _finishEntrance() {
    if (!_entrance.isCompleted) _entrance.value = 1;
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _reduced = MediaQuery.disableAnimationsOf(context);
    if (_reduced || !TickerMode.valuesOf(context).enabled) {
      _finishEntrance();
    } else if (!_entrance.isAnimating && !_entrance.isCompleted) {
      _entrance.forward();
    }
  }

  @override
  void didUpdateWidget(ShioriReadingShelf oldWidget) {
    super.didUpdateWidget(oldWidget);
    final previous = oldWidget.entries.isEmpty
        ? null
        : oldWidget.entries[_active.value % oldWidget.entries.length].id;
    final identityChanged =
        oldWidget.entries.length != widget.entries.length ||
        List.generate(
          widget.entries.length,
          (i) => i,
        ).any((i) => oldWidget.entries[i].id != widget.entries[i].id);
    _pose.count = widget.entries.length;
    _pose.repeatFill = widget.repeatFill;
    if (identityChanged || oldWidget.repeatFill != widget.repeatFill) {
      final next = math.max(
        0,
        widget.entries.indexWhere((entry) => entry.id == previous),
      );
      _command++;
      _targetIndex = null;
      _hapticGesture = false;
      _pose.move(next.toDouble());
      _active.value = next;
      _window.value = next;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted && _scroll?.hasClients == true) {
          _scroll!.jumpTo(_pose.logicalFocus * _pose.pitch);
        }
      });
    }
  }

  void _scrolled() {
    if (_scroll?.hasClients != true) return;
    _pose.move(_scroll!.offset / _pose.pitch);
    final lane = _pose.logicalFocus.round();
    if (_active.value != lane) _active.value = lane;
    // Floor changes exactly at a cycle boundary. Rounded selection changes
    // half a book earlier and would miss the wrapped scene window.
    _window.value = _pose.logicalFocus.floor();
    if (!_hapticGesture || widget.entries.length < 2 || _reduced) return;
    final previous = _lastDetent ?? lane;
    final focus = _pose.logicalFocus;
    final crossed = focus > previous
        ? (focus + .12).floor()
        : focus < previous
        ? (focus - .12).ceil()
        : previous;
    if (crossed != previous) {
      _lastDetent = crossed;
      unawaited(HapticFeedback.selectionClick());
    }
  }

  void _tap(int slot) {
    if (widget.selectionMode) {
      _open(slot);
    } else if ((_pose.focus - slot).abs() < .03 &&
        _scroll?.position.isScrollingNotifier.value == false) {
      _open(slot);
    } else {
      unawaited(_centre(_pose.logicalLane(slot)));
    }
  }

  Future<void> _centre(int lane) async {
    if (widget.entries.length < 2 || _scroll?.hasClients != true) return;
    if (!widget.repeatFill) lane = lane.clamp(0, widget.entries.length - 1);
    final target = lane * _pose.pitch;
    if ((_scroll!.offset - target).abs() < .01) return;
    _finishEntrance();
    final command = ++_command;
    _targetIndex = lane;
    _lastDetent ??= _pose.logicalFocus.round();
    _hapticGesture = true;
    if (_reduced) {
      _scroll!.jumpTo(target);
    } else {
      await _scroll!.animateTo(
        target,
        duration: ShelfMaterial.select,
        curve: ShelfMaterial.selectCurve,
      );
    }
    if (mounted && command == _command) {
      _targetIndex = null;
      _hapticGesture = false;
    }
  }

  void _step(int delta) =>
      unawaited(_centre((_targetIndex ?? _active.value) + delta));

  void _open(int slot) {
    if (widget.entries.isEmpty) return;
    _finishEntrance();
    _command++;
    _targetIndex = null;
    _hapticGesture = false;
    if (_scroll?.hasClients == true) {
      (_scroll!.position as ScrollPositionWithSingleContext).goIdle();
    }
    if (!_reduced) unawaited(HapticFeedback.lightImpact());
    widget.entries[_pose.bookIndex(slot)].onOpen();
  }

  @override
  void dispose() {
    _scroll?.removeListener(_scrolled);
    _scroll?.dispose();
    _entrance.removeListener(_pose.inkChanged);
    _entrance.dispose();
    _pose.dispose();
    _active.dispose();
    _window.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (widget.entries.isEmpty) return const SizedBox.shrink();
    final dark = Theme.of(context).brightness == Brightness.dark;
    return ArchiveMotionScope(
      animation: _entrance,
      child: CustomPaint(
        painter: _ShelfBackdrop(context.shiori.color.fg),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(24, 16, 24, 20),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _ShelfSection(title: widget.title, code: widget.sectionCode),
              LayoutBuilder(
                builder: (context, constraints) {
                  final width = math.min(constraints.maxWidth, 520.0);
                  final spine = _WavePose.spineFor(width);
                  final pitch = _WavePose.pitchFor(width);
                  final cover = _WavePose.coverFor(width);
                  final height = ArchiveShelfGeometry.bodyHeight(cover);
                  _pose.configure(width, pitch);
                  _scroll ??= ScrollController(
                    initialScrollOffset: _pose.logicalFocus * pitch,
                    keepScrollOffset: false,
                  )..addListener(_scrolled);
                  final wide =
                      constraints.maxWidth >= 880 &&
                      MediaQuery.textScalerOf(context).scale(17) / 17 < 1.6;
                  final bank = SizedBox(
                    width: width,
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        SizedBox(
                          height: ArchiveShelfGeometry.rowHeight(cover, height),
                          child: NotificationListener<ScrollNotification>(
                            onNotification: (notification) {
                              if (notification is ScrollStartNotification &&
                                  notification.dragDetails != null) {
                                _finishEntrance();
                                _command++;
                                _targetIndex = null;
                                _hapticGesture = true;
                                _lastDetent = _pose.logicalFocus.round();
                              } else if (notification
                                  is ScrollEndNotification) {
                                _hapticGesture = false;
                              }
                              return false;
                            },
                            child: Scrollable(
                              key: const ValueKey('archive-file-carousel'),
                              controller: _scroll,
                              axisDirection: AxisDirection.right,
                              physics: widget.entries.length == 1
                                  ? const NeverScrollableScrollPhysics()
                                  : _ArchiveSnapPhysics(pose: _pose),
                              viewportBuilder: (context, offset) =>
                                  ValueListenableBuilder<int>(
                                    valueListenable: _window,
                                    builder: (context, _, _) {
                                      final center = _pose.focus.round();
                                      final margin = _pose.edgeSlots + 2;
                                      final first = math.max(
                                        0,
                                        center - margin,
                                      );
                                      final last = math.min(
                                        _pose.poolCount - 1,
                                        center + margin,
                                      );
                                      return _ShelfRow(
                                        offset: offset,
                                        pose: _pose,
                                        spine: spine,
                                        cover: cover,
                                        height: height,
                                        dark: dark,
                                        firstSlot: first,
                                        slotCount: last - first + 1,
                                        children: [
                                          for (
                                            var slot = first;
                                            slot <= last;
                                            slot++
                                          )
                                            _volume(
                                              slot,
                                              spine,
                                              cover,
                                              height,
                                              dark,
                                            ),
                                        ],
                                      );
                                    },
                                  ),
                            ),
                          ),
                        ),
                        if (!wide) _caption(),
                        _navigation(),
                      ],
                    ),
                  );
                  return Center(
                    child: ConstrainedBox(
                      constraints: BoxConstraints(
                        maxWidth: wide ? 1040 : width,
                      ),
                      child: Row(
                        children: [
                          bank,
                          if (wide) ...[
                            const SizedBox(width: 48),
                            Expanded(child: _caption(wide: true)),
                          ],
                        ],
                      ),
                    ),
                  );
                },
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _caption({bool wide = false}) => ValueListenableBuilder<int>(
    valueListenable: _active,
    builder: (context, lane, _) {
      final entry = widget.entries[lane % widget.entries.length];
      return Semantics(
        button: true,
        label: '${widget.openLabel}：${entry.title}',
        child: InkWell(
          key: const ValueKey('archive-shelf-open'),
          onTap: () => _open(_pose.focus.round()),
          onLongPress: entry.onLongPress ?? entry.onDetails,
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 8),
            child: Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (wide) ...[
                        const _ShelfCode('READING ARCHIVE'),
                        const SizedBox(height: 16),
                      ],
                      Text(
                        entry.title,
                        maxLines: wide ? 3 : 2,
                        overflow: TextOverflow.ellipsis,
                        style: ShelfMaterial.title(context, wide: wide),
                      ),
                      const SizedBox(height: 3),
                      Text(
                        entry.subtitle,
                        maxLines: wide ? 3 : 2,
                        overflow: TextOverflow.ellipsis,
                        style: ShelfMaterial.subtitle(context),
                      ),
                      if (wide && entry.progress > 0) ...[
                        const SizedBox(height: 24),
                        LinearProgressIndicator(
                          value: entry.progress.clamp(0, 1),
                          minHeight: 2,
                        ),
                      ],
                    ],
                  ),
                ),
                IconButton(
                  tooltip: widget.openLabel,
                  onPressed: () => _open(_pose.focus.round()),
                  icon: const Icon(Icons.north_east, size: 20),
                ),
              ],
            ),
          ),
        ),
      );
    },
  );

  Widget _navigation() => ValueListenableBuilder<int>(
    valueListenable: _active,
    builder: (context, lane, _) {
      if (widget.entries.length < 2) return const SizedBox(height: 24);
      return Row(
        children: [
          const Spacer(),
          IconButton(
            tooltip: MaterialLocalizations.of(context).previousPageTooltip,
            onPressed: widget.repeatFill || lane > 0 ? () => _step(-1) : null,
            icon: const Icon(Icons.chevron_left, size: 17),
          ),
          Semantics(
            liveRegion: true,
            label:
                '${lane % widget.entries.length + 1} / ${widget.entries.length}',
            child: _ShelfCode(
              '${(lane % widget.entries.length + 1).toString().padLeft(2, '0')} / ${widget.entries.length.toString().padLeft(2, '0')}',
            ),
          ),
          IconButton(
            tooltip: MaterialLocalizations.of(context).nextPageTooltip,
            onPressed: widget.repeatFill || lane + 1 < widget.entries.length
                ? () => _step(1)
                : null,
            icon: const Icon(Icons.chevron_right, size: 17),
          ),
        ],
      );
    },
  );

  Widget _volume(
    int slot,
    double spine,
    double cover,
    double height,
    bool dark,
  ) {
    final index = _pose.bookIndex(slot);
    final entry = widget.entries[index];
    void tap() => _tap(slot);
    final paper = dark ? ShelfMaterial.darkSurface : ShelfMaterial.milk;
    Widget face(Widget child, {bool isSpine = false}) => ExcludeSemantics(
      child: RepaintBoundary(
        child: ArchiveShelfInk(
          animation: _entrance,
          start: _pose.inkStart(slot),
          end: _pose.inkStart(slot) + .35,
          base: isSpine
              ? (dark ? ShelfMaterial.darkRaised : ShelfMaterial.spine)
              : paper,
          ink: context.shiori.color.fg,
          spine: isSpine,
          child: Material(
            color: isSpine
                ? (dark ? ShelfMaterial.darkRaised : ShelfMaterial.spine)
                : paper,
            child: InkWell(
              onTap: tap,
              onLongPress: entry.onLongPress ?? entry.onDetails,
              onSecondaryTap: entry.onDetails,
              child: child,
            ),
          ),
        ),
      ),
    );
    return _BookVolume(
      key: ValueKey('archive-slot-$slot'),
      pose: _pose,
      index: slot,
      spine: spine,
      cover: cover,
      height: height,
      dark: dark,
      label: entry.title,
      direction: Directionality.of(context),
      onTap: tap,
      children: [
        face(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 1, vertical: 9),
            child: MediaQuery.withNoTextScaling(
              child: Column(
                children: [
                  FittedBox(child: _ShelfCode('${index + 1}'.padLeft(2, '0'))),
                  const SizedBox(height: 10),
                  Expanded(child: Center(child: _SpineTitle(entry.title))),
                  const SizedBox(height: 6),
                  Icon(
                    entry.selected ? Icons.check_circle : Icons.circle_outlined,
                    size: entry.selected ? 11 : 6,
                    color: context.shiori.color.fgMuted,
                  ),
                ],
              ),
            ),
          ),
          isSpine: true,
        ),
        face(
          Padding(
            padding: const EdgeInsets.fromLTRB(10, 9, 10, 8),
            child: Column(
              children: [
                Align(
                  alignment: Alignment.centerLeft,
                  child: _ShelfCode(
                    'NO.${(index + 1).toString().padLeft(3, '0')}',
                  ),
                ),
                const SizedBox(height: 7),
                Expanded(
                  child: _ShelfCover(
                    entry: entry,
                    pose: _pose,
                    slot: slot,
                    width: cover - 20,
                    height: (cover - 20) * 166 / 118,
                  ),
                ),
                const SizedBox(height: 4),
                _ShelfCode('YJ-${(index + 1).toString().padLeft(5, '0')}'),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

/// Only the focused copy has a Hero. Only nearby covers are decoded.
class _ShelfCover extends StatefulWidget {
  const _ShelfCover({
    required this.entry,
    required this.pose,
    required this.slot,
    required this.width,
    required this.height,
  });
  final ShioriReadingEntry entry;
  final _WavePose pose;
  final int slot;
  final double width, height;
  @override
  State<_ShelfCover> createState() => _ShelfCoverState();
}

class _ShelfCoverState extends State<_ShelfCover> {
  late bool _near, _selected;
  void _readPose() {
    _near = (widget.slot - widget.pose.focus).abs() <= 3;
    _selected = widget.slot == widget.pose.focus.round();
  }

  void _changed() {
    final near = (widget.slot - widget.pose.focus).abs() <= 3;
    final selected = widget.slot == widget.pose.focus.round();
    if (near != _near || selected != _selected) setState(_readPose);
  }

  @override
  void initState() {
    super.initState();
    _readPose();
    widget.pose.addListener(_changed);
  }

  @override
  void didUpdateWidget(_ShelfCover oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.pose != widget.pose) {
      oldWidget.pose.removeListener(_changed);
      widget.pose.addListener(_changed);
    }
    _readPose();
  }

  @override
  void dispose() {
    widget.pose.removeListener(_changed);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final fallback = ColoredBox(
      color: context.shiori.color.fg,
      child: Center(
        child: Padding(
          padding: const EdgeInsets.all(8),
          child: Text(
            widget.entry.title,
            textScaler: TextScaler.noScaling,
            maxLines: 4,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(color: context.shiori.color.ink0, fontSize: 13),
          ),
        ),
      ),
    );
    final provider = widget.entry.image;
    final decoded = provider == null
        ? null
        : ResizeImage.resizeIfNeeded(
            (widget.width * MediaQuery.devicePixelRatioOf(context))
                .ceil()
                .clamp(1, 1536),
            null,
            provider,
          );
    final image = _near && decoded != null
        ? Image(
            image: decoded,
            fit: BoxFit.cover,
            gaplessPlayback: true,
            errorBuilder: (_, _, _) => fallback,
          )
        : fallback;
    final face = ClipRRect(
      borderRadius: BorderRadius.circular(3),
      child: Stack(
        fit: StackFit.expand,
        children: [
          image,
          if (widget.entry.selected)
            Positioned(
              top: 3,
              right: 3,
              child: DecoratedBox(
                key: ValueKey('archive-selected-${widget.entry.id}'),
                decoration: BoxDecoration(
                  color: context.shiori.color.ink0,
                  shape: BoxShape.circle,
                ),
                child: Icon(
                  Icons.check_circle,
                  color: context.shiori.color.fg,
                  size: 20,
                ),
              ),
            ),
        ],
      ),
    );
    return widget.entry.heroTag != null && _selected
        ? ShioriPaperCoverHero(
            tag: widget.entry.heroTag!,
            image: decoded,
            faceSize: Size(widget.width, widget.height),
            child: face,
          )
        : face;
  }
}

class _SpineTitle extends StatelessWidget {
  const _SpineTitle(this.title);
  final String title;
  @override
  Widget build(BuildContext context) {
    final runs = <String>[];
    var latin = '';
    void flush() {
      if (latin.trim().isNotEmpty) runs.add(latin.trim());
      latin = '';
    }

    for (final glyph in title.characters) {
      if (RegExp(r'^[\x20-\x7e]+$').hasMatch(glyph)) {
        latin += glyph;
      } else {
        flush();
        runs.add(glyph);
      }
    }
    flush();
    final style = TextStyle(
      color: context.shiori.color.fg,
      fontSize: 9,
      height: 1.08,
      fontWeight: FontWeight.w600,
    );
    return FittedBox(
      fit: BoxFit.scaleDown,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final run in runs.take(7))
            RegExp(r'^[\x20-\x7e]+$').hasMatch(run)
                ? RotatedBox(
                    quarterTurns: 1,
                    child: Text(
                      run.characters.length > 24
                          ? '${run.characters.take(23)}…'
                          : run,
                      maxLines: 1,
                      style: style,
                    ),
                  )
                : Text(run, style: style, textAlign: TextAlign.center),
          if (runs.length > 7) Text('…', style: style),
        ],
      ),
    );
  }
}

abstract final class ShelfMaterial {
  static const milk = Color(0xFFFEFDF8), spine = Color(0xFFF2F0E9);
  static const darkSurface = Color(0xFF242424), darkRaised = Color(0xFF2D2D2D);
  static const darkRaisedHigh = Color(0xFF3A3A3A), darkMilk = Color(0xFFF6F6F6);
  static const entrance = Duration(milliseconds: 520);
  static const select = Duration(milliseconds: 420);
  static const selectCurve = Cubic(.2, 1.06, .35, 1);
  static TextStyle code(BuildContext context) => TextStyle(
    fontFamily: 'monospace',
    fontSize: 9,
    height: 1.35,
    letterSpacing: 1.05,
    fontWeight: FontWeight.w500,
    color: context.shiori.color.fgMuted,
  );
  static TextStyle title(BuildContext context, {bool wide = false}) =>
      context.shiori.text.section.copyWith(
        fontSize: wide ? 26 : 17,
        height: 1.35,
        fontWeight: FontWeight.w700,
        color: context.shiori.color.fg,
      );
  static TextStyle subtitle(BuildContext context) =>
      context.shiori.caption.copyWith(fontSize: 12, height: 1.4);
}

class _ShelfCode extends StatelessWidget {
  const _ShelfCode(this.text);
  final String text;
  @override
  Widget build(BuildContext context) => ExcludeSemantics(
    child: Text(
      text,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      textScaler: TextScaler.noScaling,
      style: ShelfMaterial.code(context),
    ),
  );
}

class _ShelfSection extends StatelessWidget {
  const _ShelfSection({required this.title, required this.code});
  final String title, code;
  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Row(
        children: [
          ArchivePart(start: .1, end: .68, child: _ShelfCode(code)),
          const SizedBox(width: 9),
          const Expanded(
            child: ArchivePart(
              start: 0,
              end: .46,
              child: Divider(height: 1, thickness: .6),
            ),
          ),
          const SizedBox(width: 5),
          SizedBox(
            width: 3,
            height: 3,
            child: ColoredBox(color: context.shiori.color.fg),
          ),
        ],
      ),
      const SizedBox(height: 7),
      ArchivePart(
        start: .16,
        end: .82,
        child: Text(
          title,
          style: ShelfMaterial.title(context)
              .copyWith(fontSize: 18, height: 1.25),
        ),
      ),
    ],
  );
}

class _ShelfBackdrop extends CustomPainter {
  const _ShelfBackdrop(this.color);
  final Color color;
  @override
  void paint(Canvas canvas, Size size) {
    final bottom = math.min(size.height, 240.0);
    final pen = Paint()
      ..color = color.withValues(alpha: .045)
      ..strokeWidth = .5;
    for (double x = 24; x < size.width; x += 48) {
      canvas.drawLine(Offset(x, 0), Offset(x, bottom), pen);
    }
    for (double y = 4; y < bottom; y += 48) {
      canvas.drawLine(Offset(24, y), Offset(size.width - 24, y), pen);
    }
  }

  @override
  bool shouldRepaint(_ShelfBackdrop oldDelegate) => color != oldDelegate.color;
}
