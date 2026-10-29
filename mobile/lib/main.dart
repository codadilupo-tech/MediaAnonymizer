import 'dart:io';

import 'package:flutter/material.dart';
import 'package:gal/gal.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import 'src/app.dart';
import 'src/engine/engine.dart';
import 'src/queue.dart';
import 'src/settings.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final settings = await SettingsController.load();
  final queue = QueueController(
    engine: const Engine(),
    outputDirectory: () async => Directory(p.join((await getApplicationSupportDirectory()).path, 'anonymized')),
    saveToGallery: _saveToGallery,
  );
  runApp(MediaAnonymizerApp(settings: settings, queue: queue));
}

Future<void> _saveToGallery(String path) async {
  if (!await Gal.hasAccess() && !await Gal.requestAccess()) {
    throw const _AccessDenied();
  }
  await Gal.putImage(path);
}

class _AccessDenied implements Exception {
  const _AccessDenied();

  @override
  String toString() => 'Access to the photo library was denied. Allow it in the system settings.';
}
