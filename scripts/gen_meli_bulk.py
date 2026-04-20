#!/usr/bin/env python3
"""
Generate 100k MercadoLibre-format items from the products CSV.
Outputs 10 JSONL files of 10k items each in the given output directory.
"""
import csv, json, random, hashlib, math, os, sys
from datetime import datetime, timedelta, timezone

SITE_ID = "MLA"
CURRENCY = "ARS"
ITEMS_TOTAL = 100_000
BATCH_SIZE = 10_000
SEED = 42

# MLA category IDs (real-ish format: MLA + numeric)
CATEGORY_MAP = {
    "Herramientas":                  "MLA1574",
    "Celulares y Telefonía":         "MLA1051",
    "Electrónica, Audio y Video":    "MLA1000",
    "Hogar, Muebles y Jardín":       "MLA9240",
    "Belleza y Cuidado Personal":    "MLA1246",
    "Electrodomésticos":             "MLA1574",
    "Salud y Equipamiento Médico":   "MLA1276",
    "Ropa, Bolsas y Calzado":        "MLA1430",
    "Deportes y Fitness":            "MLA1276",
    "Industrias y Oficinas":         "MLA1459",
    "Alimentos y Bebidas":           "MLA1132",
    "Juegos y Juguetes":             "MLA1132",
    "Libros, Revistas y Comics":     "MLA1459",
    "Música, Películas y Series":    "MLA1000",
    "Bebés":                         "MLA5726",
    "Automotor":                     "MLA1743",
    "Animales y Mascotas":           "MLA1132",
}

LISTING_TYPES   = ["gold_pro", "gold_special", "gold", "silver", "bronze", "free"]
CONDITIONS      = ["new", "used", "not_specified"]
BUYING_MODES    = ["buy_it_now", "auction"]
SHIPPING_MODES  = ["me2", "me1", "custom", "not_specified"]
LOGISTIC_TYPES  = ["fulfillment", "cross_docking", "drop_off", "not_specified"]
DOMAINS = {
    "MLA1051": "MLA-CELLPHONES",
    "MLA1000": "MLA-TELEVISIONS",
    "MLA1574": "MLA-POWER_TOOLS",
    "MLA9240": "MLA-FURNITURE",
    "MLA1246": "MLA-PERFUMES",
    "MLA1276": "MLA-GYM_WEIGHTS",
    "MLA1430": "MLA-SNEAKERS",
    "MLA1459": "MLA-OFFICE_CHAIRS",
    "MLA1132": "MLA-FOOD",
    "MLA5726": "MLA-BABY_CLOTHING",
    "MLA1743": "MLA-CAR_ACCESSORIES",
}

ATTR_TEMPLATES = {
    "MLA1430": [  # Ropa / Calzado
        ("BRAND",      "Marca",         None),
        ("GENDER",     "Género",        ["Hombre", "Mujer", "Unisex"]),
        ("COLOR",      "Color",         ["Negro", "Blanco", "Rojo", "Azul", "Verde", "Gris", "Rosa"]),
        ("SIZE",       "Talle",         ["36", "37", "38", "39", "40", "41", "42", "43", "44", "45"]),
        ("FOOTWEAR_TYPE", "Tipo",       ["Zapatilla", "Bota", "Sandalia", "Mocasín"]),
        ("ITEM_CONDITION", "Condición", ["Nuevo", "Usado"]),
        ("AGE_GROUP",  "Edad",          ["Adultos", "Niños", "Bebés"]),
    ],
    "MLA1051": [  # Celulares
        ("BRAND",      "Marca",         None),
        ("MODEL",      "Modelo",        None),
        ("STORAGE_CAPACITY", "Almacenamiento", ["64 GB", "128 GB", "256 GB", "512 GB"]),
        ("RAM",        "RAM",           ["4 GB", "6 GB", "8 GB", "12 GB", "16 GB"]),
        ("COLOR",      "Color",         ["Negro", "Blanco", "Azul", "Verde", "Plateado"]),
        ("ITEM_CONDITION", "Condición", ["Nuevo", "Reacondicionado"]),
        ("CONNECTIVITY", "Conectividad", ["4G", "5G"]),
    ],
    "MLA1000": [  # Electrónica
        ("BRAND",      "Marca",         None),
        ("COLOR",      "Color",         ["Negro", "Blanco", "Plateado"]),
        ("ITEM_CONDITION", "Condición", ["Nuevo", "Reacondicionado"]),
        ("VOLTAGE",    "Voltaje",       ["110V", "220V", "110V/220V"]),
    ],
    "MLA1574": [  # Herramientas / Electrodomésticos
        ("BRAND",      "Marca",         None),
        ("VOLTAGE",    "Voltaje",       ["110V", "220V", "110V/220V"]),
        ("POWER",      "Potencia",      ["500W", "750W", "1000W", "1400W", "1800W", "2000W"]),
        ("ITEM_CONDITION", "Condición", ["Nuevo"]),
        ("COLOR",      "Color",         ["Negro", "Rojo", "Amarillo", "Naranja", "Gris"]),
    ],
    "MLA9240": [  # Hogar
        ("BRAND",      "Marca",         None),
        ("COLOR",      "Color",         ["Negro", "Blanco", "Gris", "Beige", "Marrón"]),
        ("MATERIAL",   "Material",      ["Madera", "Metal", "Plástico", "Tela", "Cuero"]),
        ("ITEM_CONDITION", "Condición", ["Nuevo", "Usado"]),
    ],
    "DEFAULT": [
        ("BRAND",      "Marca",         None),
        ("ITEM_CONDITION", "Condición", ["Nuevo", "Usado"]),
        ("COLOR",      "Color",         ["Negro", "Blanco", "Gris", "Azul", "Rojo"]),
    ],
}

def load_products(path):
    products = []
    with open(path, encoding="utf-8", errors="replace") as f:
        reader = csv.DictReader(f)
        for row in reader:
            try:
                price_raw = row["Price"].replace(",", "").strip()
                price = float(price_raw) if price_raw else 0.0
                if price <= 0:
                    continue
                products.append({
                    "title":       row["Product"].strip(),
                    "brand":       row["Marca"].strip() or row["Seller"].strip(),
                    "description": row["Description"].strip() if row["Description"] else "",
                    "price":       price,
                    "category":    row["Category"].strip(),
                    "seller":      row["Seller"].strip(),
                    "stars":       row["Stars"].strip(),
                    "shipping":    row["Shipping"].strip(),
                    "discount":    row["Discount"].strip(),
                })
            except Exception:
                continue
    return products

def make_item(base, item_num, rng):
    category_id = CATEGORY_MAP.get(base["category"], "MLA1132")
    domain_id   = DOMAINS.get(category_id, "MLA-OTHER")

    # Numeric ID: deterministic from item_num so it's reproducible
    numeric_id = 1000000000 + item_num
    mla_id     = f"{SITE_ID}{numeric_id}"

    # Price with some variance (±30%)
    price = round(base["price"] * rng.uniform(0.7, 1.3), 2)
    # Convert MXN-ish to ARS (rough 1:5)
    price = round(price * 5, 2)

    condition      = rng.choices(["new", "used"], weights=[85, 15])[0]
    listing_type   = rng.choice(LISTING_TYPES)
    buying_mode    = rng.choices(BUYING_MODES, weights=[90, 10])[0]
    free_shipping  = base["shipping"] == "Free Shipping"
    shipping_mode  = rng.choice(["me2", "me1"]) if free_shipping else rng.choice(SHIPPING_MODES)
    logistic_type  = "fulfillment" if free_shipping else rng.choice(LOGISTIC_TYPES)

    quantity       = rng.randint(1, 100)
    sold           = rng.randint(0, quantity * 3)
    health         = round(rng.uniform(0.5, 1.0), 2)

    # Dates
    base_date  = datetime(2020, 1, 1, tzinfo=timezone.utc) + timedelta(days=rng.randint(0, 1500))
    stop_date  = datetime(2040, 1, 1, tzinfo=timezone.utc) + timedelta(days=rng.randint(0, 3650))
    updated    = base_date + timedelta(days=rng.randint(0, 400))
    fmt        = lambda d: d.strftime("%Y-%m-%dT%H:%M:%S.000Z")

    # Thumbnail
    thumb_id   = f"{rng.randint(100000, 999999)}-{SITE_ID}{numeric_id}_{rng.randint(1,99):02d}2024"
    thumbnail  = f"http://http2.mlstatic.com/D_{thumb_id}-I.jpg"

    # Attributes
    attr_template = ATTR_TEMPLATES.get(category_id, ATTR_TEMPLATES["DEFAULT"])
    attributes = []
    attr_counter = 1
    for attr_id, attr_name, options in attr_template:
        if attr_id == "BRAND":
            value_name = base["brand"] if base["brand"] and base["brand"] != "Visita la Tienda oficial" else "Genérico"
        elif options:
            value_name = rng.choice(options)
        else:
            value_name = None

        if value_name:
            value_id_hash = hashlib.md5(f"{attr_id}{value_name}".encode()).hexdigest()[:7]
            attributes.append({
                "id":         attr_id,
                "name":       attr_name,
                "value_id":   value_id_hash,
                "value_name": value_name,
            })
        attr_counter += 1

    # Variations (only for clothing/footwear)
    variations = []
    if category_id == "MLA1430":
        sizes  = rng.sample(["37", "38", "39", "40", "41", "42", "43"], k=rng.randint(2, 5))
        colors = rng.sample(["Negro", "Blanco", "Rojo", "Azul"], k=rng.randint(1, 2))
        var_id = 170000000000 + item_num * 10
        for color in colors:
            for size in sizes:
                variations.append({
                    "id": var_id,
                    "price": price,
                    "attribute_combinations": [
                        {"id": "COLOR", "name": "Color",
                         "value_id": hashlib.md5(color.encode()).hexdigest()[:7],
                         "value_name": color},
                        {"id": "SIZE",  "name": "Talle",
                         "value_id": hashlib.md5(size.encode()).hexdigest()[:7],
                         "value_name": f"{size},0 AR"},
                    ],
                    "available_quantity": rng.randint(0, 20),
                    "sold_quantity":      rng.randint(0, 50),
                    "sale_terms":         [],
                    "picture_ids":        [thumb_id],
                    "catalog_product_id": None,
                })
                var_id += 1

    tags = []
    if health > 0.8:
        tags.append("good_quality_picture")
        tags.append("good_quality_thumbnail")
    if rng.random() > 0.5:
        tags.append("immediate_payment")
    if free_shipping:
        tags.append("free_shipping")

    permalink = f"https://articulo.mercadolibre.com.ar/{SITE_ID}-{numeric_id}-{base['title'].lower()[:40].replace(' ', '-').replace(',', '')}_JM"

    return {
        "id":                         mla_id,
        "site_id":                    SITE_ID,
        "title":                      base["title"],
        "seller_id":                  rng.randint(100000000, 999999999),
        "category_id":                category_id,
        "official_store_id":          rng.randint(1, 5000) if rng.random() > 0.8 else None,
        "price":                      price,
        "base_price":                 price,
        "original_price":             round(price * rng.uniform(1.1, 2.0), 2) if rng.random() > 0.5 else None,
        "currency_id":                CURRENCY,
        "initial_quantity":           quantity,
        "buying_mode":                buying_mode,
        "listing_type_id":            listing_type,
        "condition":                  condition,
        "permalink":                  permalink,
        "thumbnail_id":               thumb_id,
        "thumbnail":                  thumbnail,
        "secure_thumbnail":           thumbnail.replace("http://", "https://"),
        "pictures": [{
            "id":          thumb_id,
            "url":         thumbnail.replace("-I.jpg", "-O.jpg"),
            "secure_url":  thumbnail.replace("http://", "https://").replace("-I.jpg", "-O.jpg"),
            "size":        rng.choice(["500x500", "640x480", "800x600", "1000x1000"]),
            "max_size":    "1200x1200",
            "quality":     "",
        }],
        "video_id":                   None,
        "descriptions":               [],
        "accepts_mercadopago":        True,
        "non_mercado_pago_payment_methods": [],
        "shipping": {
            "mode":          shipping_mode,
            "methods":       [],
            "tags":          ["fulfillment"] if free_shipping else [],
            "dimensions":    None,
            "local_pick_up": rng.random() > 0.7,
            "free_shipping": free_shipping,
            "logistic_type": logistic_type,
            "store_pick_up": False,
        },
        "international_delivery_mode": "none",
        "seller_address":              {"id": 0},
        "seller_contact":              None,
        "location":                    {},
        "coverage_areas":              [],
        "attributes":                  attributes,
        "listing_source":              "",
        "variations":                  variations,
        "status":                      "active",
        "sub_status":                  [],
        "tags":                        tags,
        "warranty":                    f"{rng.randint(3, 24)} meses de garantía" if rng.random() > 0.3 else None,
        "catalog_product_id":          None,
        "domain_id":                   domain_id,
        "parent_item_id":              None,
        "deal_ids":                    [],
        "automatic_relist":            rng.random() > 0.5,
        "date_created":                fmt(base_date),
        "last_updated":                fmt(updated),
        "total_listing_fee":           None,
        "health":                      health,
        "catalog_listing":             False,
        "bundle":                      None,
    }

def main():
    csv_path = "/mnt/d/Downloads/mercadolibre_products_extended.txt"
    out_dir  = "/mnt/d/Downloads/meli_bulk"

    os.makedirs(out_dir, exist_ok=True)

    print(f"Leyendo {csv_path}...", flush=True)
    products = load_products(csv_path)
    print(f"  {len(products)} productos base cargados", flush=True)

    rng = random.Random(SEED)
    num_batches = math.ceil(ITEMS_TOTAL / BATCH_SIZE)

    total_written = 0
    for batch_idx in range(num_batches):
        out_path = os.path.join(out_dir, f"batch_{batch_idx+1:02d}.jsonl")
        batch_count = min(BATCH_SIZE, ITEMS_TOTAL - total_written)
        with open(out_path, "w", encoding="utf-8") as f:
            for i in range(batch_count):
                item_num = total_written + i + 1
                base = products[(item_num - 1) % len(products)]
                item = make_item(base, item_num, rng)
                f.write(json.dumps(item, ensure_ascii=False) + "\n")
        total_written += batch_count
        size_mb = os.path.getsize(out_path) / (1024 * 1024)
        print(f"  {out_path}  ({batch_count} items, {size_mb:.1f} MB)", flush=True)

    total_mb = sum(
        os.path.getsize(os.path.join(out_dir, f))
        for f in os.listdir(out_dir)
    ) / (1024 * 1024)
    print(f"\nListo: {total_written:,} items en {out_dir}  — {total_mb:.1f} MB total")

if __name__ == "__main__":
    main()
