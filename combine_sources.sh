#!/bin/bash

OUTPUT_FILE="/web/deltamud/deltamud_all_source.c"

echo "// DeltaMUD Complete Source Code" > "$OUTPUT_FILE"
echo "// Generated on $(date)" >> "$OUTPUT_FILE"
echo "// Total files: $(find /web/deltamud/src -name "*.c" -type f | wc -l)" >> "$OUTPUT_FILE"
echo "// ============================================" >> "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Process main source files
echo "Processing main source files..."
for file in /web/deltamud/src/*.c; do
    if [ -f "$file" ]; then
        filename=$(basename "$file")
        echo "" >> "$OUTPUT_FILE"
        echo "// ============================================" >> "$OUTPUT_FILE"
        echo "// FILE: src/$filename" >> "$OUTPUT_FILE"
        echo "// Lines: $(wc -l < "$file")" >> "$OUTPUT_FILE"
        echo "// ============================================" >> "$OUTPUT_FILE"
        echo "" >> "$OUTPUT_FILE"
        cat "$file" >> "$OUTPUT_FILE"
        echo "  Added: $filename"
    fi
done

# Process utility files
echo "Processing utility files..."
for file in /web/deltamud/src/util/*.c; do
    if [ -f "$file" ]; then
        filename=$(basename "$file")
        echo "" >> "$OUTPUT_FILE"
        echo "// ============================================" >> "$OUTPUT_FILE"
        echo "// FILE: src/util/$filename" >> "$OUTPUT_FILE"
        echo "// Lines: $(wc -l < "$file")" >> "$OUTPUT_FILE"
        echo "// ============================================" >> "$OUTPUT_FILE"
        echo "" >> "$OUTPUT_FILE"
        cat "$file" >> "$OUTPUT_FILE"
        echo "  Added: util/$filename"
    fi
done

echo ""
echo "Combined file created: $OUTPUT_FILE"
echo "Total size: $(wc -l < "$OUTPUT_FILE") lines, $(wc -c < "$OUTPUT_FILE") bytes"