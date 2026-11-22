import 'package:flex_color_scheme/flex_color_scheme.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:material_ui/material_ui.dart';
import 'package:shiori/shiori.dart';
import 'package:shiori/src/components/archive_shelf/archive_scroll_simulation.dart';
import 'package:shiori/src/components/archive_shelf/paper_cover.dart';

void main() {
  List<ShioriReadingEntry> entries(List<int> opened, {bool reversed = false}) {
    final items = [
      for (var i = 1; i <= 3; i++)
        ShioriReadingEntry(
          id: i,
          title: 'Book $i — a title long enough to wrap',
          subtitle: 'Chapter $i · page 12',
          progress: i / 3,
          onOpen: () => opened.add(i),
        ),
    ];
    return reversed ? items.reversed.toList() : items;
  }

  Widget host(
    List<ShioriReadingEntry> items, {
    double scale = 1,
    bool reduced = false,
  }) => MaterialApp(
    theme: buildShioriTheme(
      brightness: Brightness.light,
      colors: FlexColor.schemesList[2].light,
      tintSurfaces: false,
      blendLevel: 10,
      density: ShioriDensity.phone,
    ),
    home: MediaQuery(
      data: MediaQueryData(
        textScaler: TextScaler.linear(scale),
        disableAnimations: reduced,
      ),
      child: Scaffold(
        body: SingleChildScrollView(
          child: ShioriReadingShelf(
            entries: items,
            title: 'Continue reading',
            openLabel: 'Resume',
          ),
        ),
      ),
    ),
  );

  testWidgets('resumes the selected book and retains identity after reorder', (
    tester,
  ) async {
    final opened = <int>[];
    await tester.pumpWidget(host(entries(opened)));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Resume'));
    expect(opened, [1]);
    await tester.ensureVisible(find.byTooltip('Next page'));
    await tester.tap(find.byTooltip('Next page'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Resume'));
    expect(opened, [1, 2]);
    await tester.pumpWidget(host(entries(opened, reversed: true)));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Resume'));
    expect(opened, [1, 2, 2]);
    expect(tester.takeException(), isNull);
  });

  testWidgets(
    'narrow split window, 200 percent text and resize remain usable',
    (tester) async {
      tester.view.devicePixelRatio = 1;
      tester.view.physicalSize = const Size(320, 640);
      addTearDown(tester.view.reset);
      await tester.pumpWidget(host(entries([]), scale: 2));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      tester.view.physicalSize = const Size(1024, 768);
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      expect(find.byTooltip('Resume'), findsOneWidget);
    },
  );

  testWidgets('removing every history item leaves no stale open action', (
    tester,
  ) async {
    await tester.pumpWidget(host(entries([])));
    await tester.pumpWidget(host([]));
    await tester.pumpAndSettle();
    expect(find.byTooltip('Resume'), findsNothing);
    expect(tester.takeException(), isNull);
  });

  ArchiveShelfSceneRender scene(WidgetTester tester) =>
      tester.allRenderObjects.whereType<ArchiveShelfSceneRender>().toSet().single;

  testWidgets('fling reverses, settles on a book and stays idle', (
    tester,
  ) async {
    await tester.pumpWidget(host(entries([])));
    await tester.pumpAndSettle();
    final shelf = find.byKey(const ValueKey('archive-file-carousel'));
    final before = scene(tester).debugFocus;
    await tester.fling(shelf, const Offset(-150, 0), 900);
    await tester.pumpAndSettle();
    final forward = scene(tester).debugFocus;
    expect(forward, greaterThan(before));
    expect(forward, closeTo(forward.roundToDouble(), .001));
    await tester.fling(shelf, const Offset(130, 0), 750);
    await tester.pumpAndSettle();
    final reverse = scene(tester).debugFocus;
    expect(reverse, lessThan(forward));
    expect(reverse, closeTo(reverse.roundToDouble(), .001));
    expect(tester.binding.transientCallbackCount, 0);
    final layouts = scene(tester).debugLayoutCount;
    await tester.pump(const Duration(seconds: 5));
    expect(scene(tester).debugFocus, reverse);
    expect(scene(tester).debugLayoutCount, layouts);
    expect(tester.takeException(), isNull);
  });

  testWidgets('a thousand books build a bounded window across the loop seam', (
    tester,
  ) async {
    final opened = <int>[];
    final items = [
      for (var i = 0; i < 1000; i++)
        ShioriReadingEntry(
          id: i,
          title: 'Book $i',
          subtitle: 'Chapter 1',
          onOpen: () => opened.add(i),
        ),
    ];
    await tester.pumpWidget(host(items));
    await tester.pumpAndSettle();
    final position = tester
        .state<ScrollableState>(
          find.byKey(const ValueKey('archive-file-carousel')),
        )
        .position;
    final pitch = scene(tester).debugPitch;
    for (final focus in [500.0, 999.75, 1000.05, 1000.75, -0.05, -1.0]) {
      position.jumpTo(focus * pitch);
      await tester.pump();
      final render = scene(tester);
      expect(render.debugBuiltCount, lessThan(45));
      expect(render.debugBuiltSlots, contains(render.debugCentreSlot));
      expect(render.debugPaintOrder, isNotEmpty);
      expect(
        render.debugPaintOrder,
        orderedEquals([...render.debugPaintOrder]..sort()),
      );
      expect(tester.takeException(), isNull);
    }
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byTooltip('Resume'));
    await tester.tap(find.byTooltip('Resume'));
    expect(opened, [999]);
  });

  testWidgets('quick next commands wrap and resize preserves focus', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 1;
    tester.view.physicalSize = const Size(390, 844);
    addTearDown(tester.view.reset);
    final opened = <int>[];
    await tester.pumpWidget(host(entries(opened)));
    await tester.pumpAndSettle();
    for (var i = 0; i < 4; i++) {
      await tester.tap(find.byTooltip('Next page'));
      await tester.pump(const Duration(milliseconds: 50));
    }
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Resume'));
    expect(opened, [2]);
    final focus = scene(tester).debugFocus;
    tester.view.physicalSize = const Size(1024, 768);
    await tester.pumpAndSettle();
    expect(scene(tester).debugFocus, closeTo(focus, .001));
    await tester.tap(find.byTooltip('Resume'));
    expect(opened, [2, 2]);
    expect(tester.takeException(), isNull);
  });

  testWidgets(
    'one book has no dead controls; reduced motion jumps immediately',
    (tester) async {
      final opened = <int>[];
      await tester.pumpWidget(host(entries(opened).take(1).toList()));
      await tester.pumpAndSettle();
      expect(find.byTooltip('Next page'), findsNothing);
      await tester.tap(find.byTooltip('Resume'));
      expect(opened, [1]);
      await tester.pumpWidget(host(entries(opened), reduced: true));
      await tester.pump();
      await tester.ensureVisible(find.byTooltip('Next page'));
      await tester.tap(find.byTooltip('Next page'));
      await tester.pump();
      expect(scene(tester).debugFocus, 1);
      // Material button ink may still be finishing after the shelf has jumped.
      await tester.pumpAndSettle();
      expect(tester.binding.transientCallbackCount, 0);
      expect(tester.takeException(), isNull);
    },
  );

  testWidgets('cover flies to detail and returns to the same selected book', (
    tester,
  ) async {
    final navigator = GlobalKey<NavigatorState>();
    await tester.pumpWidget(
      MaterialApp(
        navigatorKey: navigator,
        theme: buildShioriTheme(
          brightness: Brightness.light,
          colors: FlexColor.schemesList[2].light,
          tintSurfaces: false,
          blendLevel: 10,
          density: ShioriDensity.phone,
        ),
        home: Scaffold(
          body: SingleChildScrollView(
            child: ShioriReadingShelf(
              title: 'Library',
              openLabel: 'Open',
              entries: [
                ShioriReadingEntry(
                  id: 1,
                  title: 'Paper flight',
                  subtitle: 'Chapter 3',
                  heroTag: 'cover-1',
                  onOpen: () => navigator.currentState!.push(
                    ShioriPaperPage<void>(
                      child: Scaffold(
                        appBar: AppBar(title: const Text('Detail')),
                        body: const Padding(
                          padding: EdgeInsets.all(24),
                          child: ShioriPaperCoverHero(
                            tag: 'cover-1',
                            isDetail: true,
                            faceSize: Size(100, 150),
                            child: SizedBox(
                              width: 100,
                              height: 150,
                              child: ColoredBox(color: Colors.blueGrey),
                            ),
                          ),
                        ),
                      ),
                    ).createRoute(navigator.currentContext!),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Open'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 250));
    expect(find.byType(PaperCoverFlight), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpAndSettle();
    expect(find.text('Detail'), findsOneWidget);
    navigator.currentState!.pop();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 250));
    expect(find.byType(PaperCoverFlight), findsOneWidget);
    await tester.pumpAndSettle();
    expect(find.byTooltip('Open'), findsOneWidget);
    expect(scene(tester).debugFocus, 0);
    expect(tester.takeException(), isNull);
    expect(tester.binding.transientCallbackCount, 0);
  });

  test('fling lands monotonically and without a velocity discontinuity', () {
    for (final velocity in [-1200.0, -120.0, 120.0, 1200.0]) {
      final sim = ArchiveScrollSimulation(
        position: 11,
        velocity: velocity,
        pitch: 27,
      );
      expect(sim.x(0), closeTo(11, .0001));
      expect(sim.dx(0), closeTo(velocity, .0001));
      var last = sim.x(0);
      for (var i = 1; i <= 400; i++) {
        final position = sim.x(i / 100);
        expect(
          (position - last) * velocity.sign,
          greaterThanOrEqualTo(-.00001),
        );
        last = position;
      }
      expect(sim.isDone(4), isTrue);
      expect(last / 27, closeTo((last / 27).roundToDouble(), .0001));
      expect(sim.dx(4), closeTo(0, .0001));
      if (sim.coastDuration > 0) {
        expect(
          sim.dx(sim.coastDuration - .00001),
          closeTo(sim.dx(sim.coastDuration + .00001), .02),
        );
      }
    }
  });
}
