#!/usr/bin/env python3
import json, random, itertools

ADJECTIVES = ["Pro", "Ultra", "Max", "Mini", "Plus", "Air", "Lite", "Elite", "Smart", "Premium"]
BRANDS = ["Samsung", "Apple", "Sony", "LG", "Philips", "Asus", "HP", "Dell", "Lenovo", "Xiaomi",
          "Bosch", "Whirlpool", "Nike", "Adidas", "Levi's", "Canon", "Nikon", "JBL", "Bose", "Logitech"]

CATEGORIES = {
    "electronica": [
        ("Smartphone", ["128GB", "256GB", "512GB"], 299, 1499),
        ("Laptop", ["i5", "i7", "Ryzen 5", "Ryzen 7"], 499, 2499),
        ("Tablet", ["WiFi", "5G", "LTE"], 199, 999),
        ("Auriculares", ["inalámbricos", "con cable", "noise cancelling"], 29, 399),
        ("Smartwatch", ["GPS", "ECG", "deportivo"], 99, 599),
        ("Monitor", ['24"', '27"', '32"', '4K'], 149, 899),
        ("Teclado", ["mecánico", "inalámbrico", "RGB"], 29, 249),
        ("Mouse", ["gaming", "ergonómico", "inalámbrico"], 15, 149),
        ("Cámara", ["mirrorless", "DSLR", "compacta"], 299, 2999),
        ("Parlante", ["Bluetooth", "portátil", "smart"], 29, 399),
    ],
    "hogar": [
        ("Heladera", ["no frost", "con freezer", "inverter"], 399, 1499),
        ("Lavarropas", ["automático", "carga frontal", "carga superior"], 299, 999),
        ("Microondas", ["digital", "con grill", "inverter"], 79, 299),
        ("Aspiradora", ["robot", "sin cable", "ciclónica"], 59, 599),
        ("Cafetera", ["espresso", "cápsulas", "French press"], 29, 499),
        ("Licuadora", ["de vaso", "de mano", "procesadora"], 19, 199),
        ("Televisor", ['43"', '55"', '65"', '75"', "OLED", "QLED"], 299, 2999),
        ("Ventilador", ["torre", "de techo", "portatil"], 29, 199),
        ("Plancha", ["vapor", "inalámbrica", "cerámica"], 19, 149),
        ("Silla", ["ergonómica", "gaming", "ejecutiva"], 49, 599),
    ],
    "ropa": [
        ("Remera", ["manga corta", "manga larga", "polo"], 9, 79),
        ("Pantalón", ["jeans", "cargo", "chino", "deportivo"], 19, 149),
        ("Zapatillas", ["running", "casual", "training", "basketball"], 39, 299),
        ("Campera", ["impermeable", "plumas", "polar", "cuero"], 49, 399),
        ("Buzo", ["hoodie", "crewneck", "zip"], 19, 129),
        ("Vestido", ["casual", "formal", "verano"], 19, 199),
        ("Calzado", ["mocasin", "bota", "sandalia", "oxford"], 29, 249),
        ("Medias", ["deportivas", "de compresión", "calzas"], 5, 49),
        ("Gorra", ["snapback", "trucker", "bucket hat"], 9, 69),
        ("Mochila", ["urbana", "hiking", "escolar"], 19, 199),
    ],
    "deportes": [
        ("Bicicleta", ["mountain bike", "ruta", "urbana", "eléctrica"], 199, 2999),
        ("Pelota", ["fútbol", "básquet", "tenis", "voley"], 9, 149),
        ("Pesas", ["mancuerna", "kettlebell", "disco"], 9, 299),
        ("Caminadora", ["plegable", "eléctrica", "manual"], 199, 1999),
        ("Colchoneta", ["yoga", "pilates", "gimnasia"], 9, 99),
        ("Guantes", ["boxeo", "ciclismo", "portero"], 9, 99),
        ("Raqueta", ["tenis", "padel", "squash"], 19, 299),
        ("Casco", ["bicicleta", "moto", "ski"], 29, 299),
        ("Bolsa de deporte", ["entrenamiento", "natación", "gym"], 9, 79),
        ("Suplemento", ["proteína", "creatina", "BCAA", "pre-workout"], 19, 149),
    ],
    "libros": [
        ("Novela", ["ficción", "thriller", "romance", "histórica"], 9, 29),
        ("Manual", ["programación", "diseño", "marketing", "finanzas"], 19, 99),
        ("Comic", ["superhéroes", "manga", "novela gráfica"], 9, 49),
        ("Infantil", ["cuentos", "educativo", "pop-up"], 5, 29),
        ("Autoayuda", ["productividad", "mindfulness", "liderazgo"], 9, 39),
        ("Historia", ["mundial", "argentina", "antigua", "moderna"], 9, 49),
        ("Ciencia", ["astronomía", "biología", "física", "química"], 19, 79),
        ("Cocina", ["pastelería", "vegana", "asados", "internacional"], 9, 49),
        ("Arte", ["fotografía", "pintura", "arquitectura"], 19, 99),
        ("Diccionario", ["español", "inglés", "bilingüe", "técnico"], 9, 59),
    ],
}

DESCRIPTIONS = [
    "Ideal para uso cotidiano y profesional.",
    "Diseño moderno con materiales de alta calidad.",
    "Rendimiento superior garantizado.",
    "Perfecta relación calidad-precio.",
    "El favorito de los expertos.",
    "Tecnología de última generación.",
    "Durabilidad comprobada y garantía extendida.",
    "Envío gratis a todo el país.",
    "Stock limitado, aprovechá la oferta.",
    "El más vendido de la categoría.",
    "Compatible con todos los sistemas.",
    "Incluye accesorios y manual en español.",
    "Certificado por normas internacionales de calidad.",
    "Recomendado por profesionales del sector.",
    "Disponible en múltiples colores y tallas.",
]

COLORS = ["Negro", "Blanco", "Gris", "Azul", "Rojo", "Verde", "Dorado", "Plateado", "Rosa", "Naranja"]

def generate_products(n=10000):
    products = []
    product_id = 1
    all_variants = []
    for category, items in CATEGORIES.items():
        for product_name, variants, price_min, price_max in items:
            for brand in BRANDS:
                for adj in ADJECTIVES:
                    all_variants.append((category, product_name, variants, price_min, price_max, brand, adj))

    random.seed(42)
    random.shuffle(all_variants)
    cycle = itertools.cycle(all_variants)

    for _ in range(n):
        category, product_name, variants, price_min, price_max, brand, adj = next(cycle)
        variant = random.choice(variants)
        color = random.choice(COLORS)
        price = round(random.uniform(price_min, price_max), 2)
        desc = random.choice(DESCRIPTIONS)
        title = f"{brand} {product_name} {adj} {variant}"

        products.append({
            "id": product_id,
            "title": title,
            "description": f"{title} - {color}. {desc}",
            "price": price,
            "category": category,
            "attributes": {
                "brand": {"String": brand},
                "color": {"String": color},
                "variant": {"String": variant},
            }
        })
        product_id += 1

    return products

if __name__ == "__main__":
    products = generate_products(10000)
    with open("products.json", "w", encoding="utf-8") as f:
        json.dump(products, f, ensure_ascii=False, indent=2)
    print(f"Generados {len(products)} productos en products.json")
    print(f"Ejemplo: {products[0]}")
