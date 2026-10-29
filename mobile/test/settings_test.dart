import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:mediaanonymizer/src/settings.dart';

void main() {
  test('defaults produce core options', () {
    final options = jsonDecode(const Settings().toOptionsJson()) as Map<String, dynamic>;
    expect(options['metadata'], 'all');
    expect(options['faces'], {
      'mode': 'full-face',
      'style': 'black-band',
      'shape': 'ellipse',
      'padding': 0.15,
      'min_confidence': 0.6,
      'strength': 1.0,
    });
    expect(options['timestamp'], isNull);
    expect(options['compute'], {'device': 'cpu'});
  });

  test('disabled features are null and enums use kebab-case ids', () {
    const s = Settings(
      metadata: MetadataPolicy.sensitive,
      faces: false,
      timestamp: true,
      timestampPosition: Corner.bottomLeft,
    );
    final options = jsonDecode(s.toOptionsJson()) as Map<String, dynamic>;
    expect(options['metadata'], 'sensitive');
    expect(options['faces'], isNull);
    expect(options['timestamp'], {'position': 'bottom-left', 'format': '%Y-%m-%d %H:%M:%S', 'size': 0.035});
  });

  test('round-trips through JSON and tolerates unknown values', () {
    const s = Settings(style: RedactionStyle.pixelate, faceMode: FaceMode.eyesOnly, strength: 2.5);
    expect(Settings.fromJson(s.toJson()), s);
    final broken = Settings.fromJson({'style': 'sparkles', 'strength': 'x', 'faces': 'yes', 'padding': 1});
    expect(broken.style, RedactionStyle.blackBand);
    expect(broken.strength, 1.0);
    expect(broken.faces, isTrue);
    expect(broken.padding, 1.0);
  });

  test('reports settings that would do nothing', () {
    expect(const Settings().problem, isNull);
    expect(const Settings(metadata: MetadataPolicy.keep, faces: false).problem, isNotNull);
    expect(const Settings(timestamp: true, timestampFormat: ' ').problem, isNotNull);
  });
}
