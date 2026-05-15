export type ScanOptions = {
  includeSubfolders: boolean;
  detectExactDuplicates: boolean;
  detectSimilarImages: boolean;
  detectBlurryImages: boolean;
};

export type ImageItem = {
  id: string;
  path: string;
  fileName: string;
  extension: string;
  sizeBytes: number;
  width: number;
  height: number;
  modifiedAt: string;
  blurScore: number | null;
};

export type ImageGroup = {
  id: string;
  title: string;
  items: ImageItem[];
};

export type ScanResult = {
  scannedCount: number;
  skippedCount: number;
  exactDuplicateGroups: ImageGroup[];
  similarImageGroups: ImageGroup[];
  blurryImages: ImageItem[];
};
