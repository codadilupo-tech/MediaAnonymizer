# /tmp/claude-0/-home-user-MediaAnonymizer/eb3fc481-ef9c-564d-aa8e-6da7a62c8a65/scratchpad/train/runs/v1/best.pt on the test split

2,861 head crops. Presence = the face's crop contains the accessory; threshold = lowest score reaching precision 90% (else best F1).

| Accessory | Boxes | Faces with it | AP@0.5 | Threshold | Precision | Recall |
|---|---:|---:|---:|---:|---:|---:|
| hat | 487 | 414 | 0.531 | 0.85 | 0.566 (below target) | 0.819 |
| helmet | 237 | 228 | 0.615 | 0.90 | 0.607 (below target) | 0.697 |
| glasses | 1,033 | 860 | 0.626 | 0.95 | 0.938 | 0.142 |
| sunglasses | 296 | 279 | 0.515 | 0.40 | 0.603 (below target) | 0.685 |

Mean AP@0.5: **0.572**

## Notes on these numbers (v1, 10 epochs on CPU)

* Thresholds embedded in `accessories.onnx` are the best-F1 ones
  (hat 0.85, helmet 0.90, glasses 0.40, sunglasses 0.40): suggestions are
  confirmed by the user, so recall matters more than precision.
* Precision is underestimated by missing labels: Open Images boxes a class
  only in images where it was checked. In a random sample of 24 "false" hat
  detections, about 18 showed real headwear; for helmets about 7 of 24 were
  real helmets (the rest: caps, swim caps, dark or blurred crops). Estimated
  true precision at score 0.7: hat about 0.85-0.9, helmet about 0.65.
* Hats and helmets are rarely confused with each other (5 of 347 hat
  false positives were helmets).
* ONNX Runtime gives the same results as PyTorch on the whole test split.
