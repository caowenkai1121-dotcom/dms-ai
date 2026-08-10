SELECT COUNT(DISTINCT sales_order_code) AS `订单数` FROM t_sales_order WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') AND DATE(order_time) = CURDATE() - INTERVAL 1 DAY LIMIT 200
