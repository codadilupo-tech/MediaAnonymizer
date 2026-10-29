import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mediaanonymizer/src/app.dart';
import 'package:mediaanonymizer/src/engine/engine.dart';
import 'package:mediaanonymizer/src/queue.dart';
import 'package:mediaanonymizer/src/settings.dart';
import 'package:mediaanonymizer/src/ui/settings_page.dart';
import 'package:shared_preferences/shared_preferences.dart';

QueueController _queue() => QueueController(
  engine: const Engine(),
  outputDirectory: () async => Directory.systemTemp,
  saveToGallery: (_) async {},
);

void main() {
  setUp(() => SharedPreferences.setMockInitialValues({}));

  testWidgets('empty queue invites to choose photos', (tester) async {
    await tester.pumpWidget(MediaAnonymizerApp(settings: SettingsController(), queue: _queue()));
    expect(find.text('Anonymize your photos'), findsOneWidget);
    expect(find.text('Choose photos'), findsOneWidget);
    expect(find.text('Take a photo'), findsOneWidget);
  });

  testWidgets('queued photos show the action bar and the settings summary', (tester) async {
    final queue = _queue()..add([PhotoItem('/nonexistent/a.jpg'), PhotoItem('/nonexistent/b.jpg')]);
    final settings = SettingsController();
    await tester.pumpWidget(MediaAnonymizerApp(settings: settings, queue: queue));
    expect(find.text('a.jpg'), findsOneWidget);
    expect(find.text('Anonymize 2 photos'), findsOneWidget);
    expect(find.textContaining('All metadata removed'), findsOneWidget);

    settings.update(const Settings(metadata: MetadataPolicy.keep, faces: false));
    await tester.pump();
    expect(find.textContaining('Nothing to do'), findsOneWidget);
    final button = tester.widget<FilledButton>(
      find.ancestor(of: find.text('Anonymize 2 photos'), matching: find.byWidgetPredicate((w) => w is FilledButton)),
    );
    expect(button.onPressed, isNull);
  });

  testWidgets('settings page edits and persists settings', (tester) async {
    final settings = SettingsController();
    await tester.pumpWidget(MaterialApp(home: SettingsPage(settings: settings)));
    await tester.tap(find.text('Remove sensitive'));
    await tester.pump();
    expect(settings.value.metadata, MetadataPolicy.sensitive);

    await tester.tap(find.text('Blur'));
    await tester.pump();
    expect(settings.value.style, RedactionStyle.blur);
    expect(find.text('Strength'), findsOneWidget);

    await tester.tap(find.widgetWithText(SwitchListTile, 'Redact faces'));
    await tester.pump();
    expect(settings.value.faces, isFalse);
    expect(find.text('Blur'), findsNothing);

    final prefs = await SharedPreferences.getInstance();
    expect(prefs.getString('settings.v1'), contains('"metadata":"sensitive"'));
    final reloaded = await SettingsController.load();
    expect(reloaded.value, settings.value);
  });
}
