
        Alias: $orange = http://example.org/StructureDefinition/Citrus#Citrus.orange
        Logical: LogicalModel
        * oranges 1..* MS contentReference $orange "oranges" "oranges are a citrus"
        * apples 0..3 contentReference http://example.org/StructureDefinition/Fruit#Fruit.apple "apples"
        